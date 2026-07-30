#!/usr/bin/env python3
"""network_leases 테이블 눈으로 확인하기.

Usage:
    python3 vm-ip-mac-allocation.py [db-path]

db-path 기본값은 data/firecrab.db (repo 루트 기준).
"""
import sqlite3
import sys

DEFAULT_DB = "data/firecrab.db"
DB_PATH = sys.argv[1] if len(sys.argv) > 1 else DEFAULT_DB

conn = sqlite3.connect(DB_PATH)
conn.row_factory = sqlite3.Row

print("== schema ==")
for row in conn.execute(
    "SELECT sql FROM sqlite_master WHERE type IN ('table', 'index') AND name LIKE 'network_leases%'"
):
    if row["sql"]:
        print(row["sql"])

print("\n== active leases ==")
for row in conn.execute(
    "SELECT vm_id, ipv4, mac, allocated_at FROM network_leases WHERE released_at IS NULL ORDER BY allocated_at"
):
    print(f"{row['vm_id']}  {row['ipv4']:>15}  {row['mac']}  {row['allocated_at']}")

print("\n== released (history) ==")
for row in conn.execute(
    "SELECT vm_id, ipv4, mac, allocated_at, released_at FROM network_leases WHERE released_at IS NOT NULL ORDER BY released_at"
):
    print(f"{row['vm_id']}  {row['ipv4']:>15}  {row['mac']}  {row['allocated_at']} -> {row['released_at']}")
