#!/bin/zsh
# Snapshots of every page (dark), with a test database open, on a fresh test store.
# Needs APP_SEED_URL (or DBLENS_TEST_URL) pointing at a throwaway Postgres.
# Usage: tools/snap.sh            all pages
#        APP_SNAPSHOT_PAGES=table,settings tools/snap.sh
set -u
app=~/Applications/dbLens.app/Contents/MacOS/dbLens
store=/tmp/dbLens-test.store
shots=/tmp/dbLens-shots
# A crashed run leaves saved window state, and the next launch would wait on a "reopen windows?" alert.
rm -rf ~/Library/Saved\ Application\ State/io.github.miguelplaza17.dblens.savedState
# Seed data only fills an empty store, so start from nothing.
rm -f $store $store-shm $store-wal
rm -rf $shots
APP_SEED_URL=${APP_SEED_URL:-${DBLENS_TEST_URL:-}} APP_STORE=$store APP_SNAPSHOT=$shots $app
