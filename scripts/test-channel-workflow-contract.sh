#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
workflow="$repo_root/.github/workflows/promote-channel.yml"
release_workflow="$repo_root/.github/workflows/release.yml"
bootstrap_workflow="$repo_root/.github/workflows/bootstrap-channels.yml"
nightly_workflow="$repo_root/.github/workflows/nightly.yml"
nightly_promotion_workflow="$repo_root/.github/workflows/promote-nightly.yml"

grep -Fq "if: github.ref == 'refs/heads/main'" "$workflow"
grep -Fq "repos/\$GITHUB_REPOSITORY/git/ref/tags/\$RELEASE_TAG" "$workflow"
grep -Fq "repos/\$GITHUB_REPOSITORY/git/tags/\$TAG_COMMIT" "$workflow"
grep -Fq "[[ \"\$TAG_TYPE\" == commit ]]" "$workflow"
grep -Fq "TRANSACTION=\"channel-\${CHANNEL}-\${GENERATION}.transaction.json\"" "$workflow"
grep -Fq "cmp \"\$TRANSACTION\" \"existing-transaction/\$TRANSACTION\"" "$workflow"
grep -Fq 'python3 scripts/channel_transaction.py unpack' "$workflow"
grep -Fq -- "--now \"\$NOW\"" "$workflow"
grep -Fq "cp transaction-floor-pair/channel.toml \"\$FLOOR_HISTORY\"" "$workflow"
grep -Fq '."history-floor"' "$workflow"
grep -Fq 'recover_current_from_transaction' "$workflow"
grep -Fq 'current pointer is unauthenticated; continuity will use authenticated transactions' "$workflow"
grep -Fq 'python3 scripts/channel_publication.py' "$workflow"
grep -Fq 'cleanup-asset-ids' "$workflow"
grep -Fq "releases/assets/\$asset_id" "$workflow"
grep -Fq 'cmp transaction-floor-pair/channel.toml.sigstore.json' "$workflow"
grep -Fq 'cmp channel.toml.sigstore.json' "$workflow"
grep -Fq 'prepublish-current/channel.toml' "$workflow"
grep -Fq "prepublish-immutable/\$TRANSACTION" "$workflow"
grep -Fq "prepublish-immutable/\$HISTORY.sigstore.json" "$workflow"
grep -Fq '.immutable == false' "$workflow"
grep -Fq 'run the protected channel bootstrap before promotion' "$workflow"
grep -Fq 'release assets are write-once' "$release_workflow"
grep -Fq 'overwrite_files: false' "$release_workflow"
grep -Fq 'draft: true' "$release_workflow"
grep -Fq -- '-F draft=false' "$release_workflow"
grep -Fq 'SELECTED_RELEASE_ID=$RELEASE_ID' "$release_workflow"
grep -Fq '[[ "$RELEASE_ID" == "$SELECTED_RELEASE_ID" ]]' "$release_workflow"
grep -Fq 'cmp "$CANDIDATE/$asset" "$FINAL/$asset"' "$release_workflow"
grep -Fq 'fail_on_unmatched_files: true' "$release_workflow"
grep -Fq "group: aos-release-\${{ github.ref }}" "$release_workflow"
grep -Fq 'RECOVER_DRAFT=1' "$release_workflow"
grep -Fq '.published_at == null' "$release_workflow"
grep -Fq 'python3 scripts/release_publication.py' "$release_workflow"
grep -Fq 'scripts/test-clean-home-init.sh' "$release_workflow"
grep -Fq 'b3sum --check BLAKE3SUMS.txt' "$release_workflow"
grep -Fq "repos/\$GITHUB_REPOSITORY/immutable-releases" "$release_workflow"
grep -Fq '.immutable == true' "$release_workflow"
grep -Fq 'AOS_RELEASE_ADMIN_TOKEN' "$release_workflow"
grep -Fq 'actions: read' "$workflow"
grep -Fq 'actions/runs/$GITHUB_RUN_ID' "$release_workflow"
grep -Fq -- "- '!20[0-9][0-9].*-nightly.*'" "$release_workflow"
grep -Fq 'git/matching-refs/tags/$BASE_VERSION' "$release_workflow"
grep -Fq 'repos/$GITHUB_REPOSITORY/git/ref/tags/$GITHUB_REF_NAME' "$release_workflow"
grep -Fq -- '-f make_latest="$EXPECTED_LATEST"' "$release_workflow"
grep -Fq 'for channel in stable dev nightly' "$bootstrap_workflow"
grep -Fq "GH_TOKEN=\"\$GH_ADMIN_TOKEN\" gh api --method PUT" "$bootstrap_workflow"
grep -Fq "repos/\$GITHUB_REPOSITORY/immutable-releases" "$bootstrap_workflow"
grep -Fq "jq -e '.enabled == true'" "$bootstrap_workflow"
grep -Fq "jq -e '.enabled == false'" "$bootstrap_workflow"
grep -Fq '.immutable == false' "$bootstrap_workflow"
grep -Fq 'environment: release' "$bootstrap_workflow"
grep -Fq 'vars.AOS_NIGHTLY_RELEASES_ENABLED' "$nightly_workflow"
grep -Fq 'actions/runs/$GITHUB_RUN_ID' "$nightly_workflow"
grep -Fq 'git/matching-refs/tags/$BASE_VERSION' "$nightly_workflow"
grep -Fq 'gh workflow run release.yml --ref "$VERSION"' "$nightly_workflow"
grep -Fq 'recover-promotion=true' "$nightly_workflow"
grep -Fq 'gh workflow run promote-channel.yml' "$nightly_workflow"
grep -Fq '.run_number] | unique | if length == 1' "$nightly_workflow"
grep -Fq 'git merge-base --is-ancestor "$SOURCE_COMMIT" origin/main' "$nightly_workflow"
grep -Fq 'git merge-base --is-ancestor "$SOURCE_COMMIT" origin/main' "$release_workflow"
grep -Fq 'prerelease: ${{ needs.classify.outputs.nightly == '\''true'\'' }}' "$release_workflow"
grep -Fq 'workflow_run:' "$nightly_promotion_workflow"
grep -Fq "github.event.workflow_run.conclusion == 'success'" "$nightly_promotion_workflow"
grep -Fq 'gh workflow run promote-channel.yml' "$nightly_promotion_workflow"

python3 - "$workflow" <<'PY'
import pathlib
import sys

text = pathlib.Path(sys.argv[1]).read_text(encoding="utf-8")
transaction_upload = text.index(
    'gh release upload "$CHANNEL_TAG" \\\n'
    '              --repo "$GITHUB_REPOSITORY" \\\n'
    '              "$TRANSACTION"'
)
history_upload = text.index(
    '[[ "$history_pointer_present" == 1 ]] || gh release upload'
)
if transaction_upload >= history_upload:
    raise SystemExit("channel transaction must be uploaded before history assets")
PY

python3 - "$release_workflow" <<'PY'
import pathlib
import sys

text = pathlib.Path(sys.argv[1]).read_text(encoding="utf-8")
start = text.index("  validate-release:\n")
end = text.index("\n  build:\n", start)
validate = text[start:end]
install = validate.index('cargo install b3sum --locked --version "$B3SUM_VERSION"')
exercise = validate.index("bash scripts/test-install.sh")
if install >= exercise:
    raise SystemExit("release identity must install b3sum before exercising the installer")
PY

if grep -Fq "repos/\$GITHUB_REPOSITORY/commits/\$RELEASE_TAG" "$workflow"; then
  echo "channel promotion resolves an ambiguous branch-or-tag revision" >&2
  exit 1
fi
