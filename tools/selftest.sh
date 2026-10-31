#!/bin/zsh
# Runs the in-app self-test against a fresh store in /tmp and a throwaway Postgres
# (APP_SEED_URL or DBLENS_TEST_URL; it creates and drops one table). Exit code 0 when every check passed.
set -u
rm -rf ~/Library/Saved\ Application\ State/io.github.miguelplaza17.dblens.savedState
store=/tmp/dbLens-self.store
rm -f $store $store-shm $store-wal
APP_SEED_URL=${APP_SEED_URL:-${DBLENS_TEST_URL:-}} APP_STORE=$store ~/Applications/dbLens.app/Contents/MacOS/dbLens --selftest
