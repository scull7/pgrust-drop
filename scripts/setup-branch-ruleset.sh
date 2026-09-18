#!/usr/bin/env bash
# Create (or update) the `main` branch ruleset that requires the CI lanes.
#
# Run this locally with the `gh` CLI authenticated as a repository admin; the
# GitHub App credentials a cloud agent gets cannot write rulesets.
#
#   scripts/setup-branch-ruleset.sh                 # create/update, enforcement active
#   scripts/setup-branch-ruleset.sh --dry-run       # print the payload and exit
#   scripts/setup-branch-ruleset.sh --require-pr --bypass-me
#   scripts/setup-branch-ruleset.sh --delete
#
# Why every lane is required, not just the last one (ADR-0007): GitHub counts a
# `skipped` check as successful, and a job skipped because its `needs:` failed
# does not block a merge. `gnu` and `apple` declare `needs: musl`, so requiring
# only `gnu` would let a red musl lane through. `musl` always runs on a pull
# request, so requiring all three is what makes the ordering enforceable.
#
# Idempotent: a ruleset with the same name is updated in place, keeping its id
# and history, so re-running after editing ci.yml is the intended workflow.
set -euo pipefail

readonly DEFAULT_RULESET_NAME="main: require CI lanes"
readonly WORKFLOW=".github/workflows/ci.yml"

repo=""
ruleset_name="$DEFAULT_RULESET_NAME"
enforcement="active"
strict=false
require_pr=false
bypass_me=false
dry_run=false
do_delete=false
checks=()

die() { echo "error: $*" >&2; exit 1; }
note() { echo "$*" >&2; }

usage() {
  sed -n '2,19p' "$0" | sed 's/^# \{0,1\}//'
  cat <<'EOF'

Options:
  --repo OWNER/NAME   Target repository (default: the current checkout's).
  --name NAME         Ruleset name (default: "main: require CI lanes").
  --check NAME        Required status check; repeatable. Default: every job in
                      .github/workflows/ci.yml.
  --enforcement WHICH active | disabled | evaluate (default: active).
                      Note the GitHub UI defaults to disabled; this does not.
  --strict            Also require branches to be up to date before merging.
  --require-pr        Also require a pull request before merging (0 approvals).
  --bypass-me         Let the authenticated user bypass the ruleset.
  --dry-run           Print the payload instead of sending it.
  --delete            Delete the ruleset with this name.
  -h, --help          This text.
EOF
}

while [ $# -gt 0 ]; do
  case "$1" in
    --repo) repo="${2:?--repo needs a value}"; shift 2 ;;
    --name) ruleset_name="${2:?--name needs a value}"; shift 2 ;;
    --check) checks+=("${2:?--check needs a value}"); shift 2 ;;
    --enforcement) enforcement="${2:?--enforcement needs a value}"; shift 2 ;;
    --strict) strict=true; shift ;;
    --require-pr) require_pr=true; shift ;;
    --bypass-me) bypass_me=true; shift ;;
    --dry-run) dry_run=true; shift ;;
    --delete) do_delete=true; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown argument: $1 (try --help)" ;;
  esac
done

case "$enforcement" in
  active|disabled|evaluate) ;;
  *) die "--enforcement must be active, disabled or evaluate" ;;
esac

command -v gh >/dev/null || die "gh is not installed: https://cli.github.com"
gh auth status >/dev/null 2>&1 || die "gh is not authenticated; run: gh auth login"

if [ -z "$repo" ]; then
  repo="$(gh repo view --json nameWithOwner --jq .nameWithOwner 2>/dev/null)" \
    || die "could not detect the repository; pass --repo OWNER/NAME"
fi

# Pure: the status-check names CI publishes are the job display names, which
# default to the job id. Reading them from the workflow keeps this script and
# ci.yml from drifting apart silently.
workflow_checks() {
  [ -f "$WORKFLOW" ] || return 0
  awk '
    /^jobs:[[:space:]]*$/ { in_jobs = 1; next }
    in_jobs && /^[^[:space:]#]/ { in_jobs = 0 }
    in_jobs && /^  [A-Za-z0-9_-]+:[[:space:]]*$/ {
      if (job != "") print label
      job = $1; sub(/:$/, "", job); label = job; next
    }
    in_jobs && job != "" && /^    name:[[:space:]]/ {
      line = $0; sub(/^    name:[[:space:]]*/, "", line)
      gsub(/^["'\'']|["'\'']$/, "", line); label = line
    }
    END { if (job != "") print label }
  ' "$WORKFLOW"
}

mapfile -t derived < <(workflow_checks)

if [ ${#checks[@]} -eq 0 ]; then
  if ! $do_delete; then
    [ ${#derived[@]} -gt 0 ] || die "no jobs found in $WORKFLOW; pass --check NAME"
    note "required checks, from $WORKFLOW: ${derived[*]}"
  fi
  checks=(${derived[@]+"${derived[@]}"})
else
  for check in "${checks[@]}"; do
    found=false
    for candidate in ${derived[@]+"${derived[@]}"}; do
      [ "$check" = "$candidate" ] && found=true
    done
    $found || note "warning: '$check' is not a job in $WORKFLOW; a check that never reports leaves pull requests stuck on \"Expected\""
  done
fi

# --- find an existing ruleset with this name ------------------------------
existing_id="$(gh api "repos/$repo/rulesets" --jq \
  ".[] | select(.name == \"$ruleset_name\") | .id" 2>/dev/null | head -1 || true)"

if $do_delete; then
  [ -n "$existing_id" ] || die "no ruleset named '$ruleset_name' in $repo"
  $dry_run && { echo "DELETE repos/$repo/rulesets/$existing_id"; exit 0; }
  gh api -X DELETE "repos/$repo/rulesets/$existing_id" --silent
  echo "deleted ruleset '$ruleset_name' ($existing_id) from $repo"
  exit 0
fi

# --- build the payload ----------------------------------------------------
contexts=""
for check in "${checks[@]}"; do
  [ -n "$contexts" ] && contexts+=","
  contexts+="{\"context\":\"$check\"}"
done

rules="{\"type\":\"required_status_checks\",\"parameters\":{\"strict_required_status_checks_policy\":$strict,\"do_not_enforce_on_create\":true,\"required_status_checks\":[$contexts]}}"

if $require_pr; then
  rules+=',{"type":"pull_request","parameters":{"required_approving_review_count":0,"dismiss_stale_reviews_on_push":false,"require_code_owner_review":false,"require_last_push_approval":false,"required_review_thread_resolution":false,"allowed_merge_methods":["merge","squash","rebase"]}}'
fi

bypass=""
if $bypass_me; then
  # A personal repository has no OrganizationAdmin actor, and RepositoryRole
  # ids are not documented, so bind the bypass to this user explicitly.
  user_id="$(gh api user --jq .id)" || die "could not read the authenticated user's id"
  bypass="{\"actor_type\":\"User\",\"actor_id\":$user_id,\"bypass_mode\":\"always\"}"
fi

payload="{\"name\":\"$ruleset_name\",\"target\":\"branch\",\"enforcement\":\"$enforcement\",\"bypass_actors\":[$bypass],\"conditions\":{\"ref_name\":{\"include\":[\"~DEFAULT_BRANCH\"],\"exclude\":[]}},\"rules\":[$rules]}"

if $dry_run; then
  if [ -n "$existing_id" ]; then
    echo "PUT repos/$repo/rulesets/$existing_id"
  else
    echo "POST repos/$repo/rulesets"
  fi
  printf '%s\n' "$payload"
  exit 0
fi

if [ -n "$existing_id" ]; then
  note "updating ruleset '$ruleset_name' ($existing_id) in $repo"
  printf '%s' "$payload" | gh api -X PUT "repos/$repo/rulesets/$existing_id" --input - >/dev/null
  ruleset_id="$existing_id"
else
  note "creating ruleset '$ruleset_name' in $repo"
  ruleset_id="$(printf '%s' "$payload" | gh api -X POST "repos/$repo/rulesets" --input - --jq .id)"
fi

# --- read back what GitHub actually stored --------------------------------
gh api "repos/$repo/rulesets/$ruleset_id" --jq '
  "ruleset:      \(.name) (id \(.id))",
  "enforcement:  \(.enforcement)",
  "targets:      \(.conditions.ref_name.include | join(", "))",
  "bypass:       \(if (.bypass_actors | length) == 0 then "none" else ([.bypass_actors[] | "\(.actor_type):\(.actor_id // "-") (\(.bypass_mode))"] | join(", ")) end)",
  "rules:        \([.rules[].type] | join(", "))",
  "checks:       \([.rules[] | select(.type == "required_status_checks") | .parameters.required_status_checks[].context] | join(", "))"
'

cat >&2 <<EOF

Done. Two things to know:
  * Direct pushes to the default branch may now be blocked: 'apple' and 'gnu'
    only run on pull requests, so a commit pushed straight to main can never
    satisfy them. Work through pull requests, or re-run with --bypass-me.
  * Re-run this script after renaming a CI job; the required check name must
    match the job's name exactly, or pull requests wait forever on "Expected".
EOF
