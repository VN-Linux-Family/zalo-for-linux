#!/usr/bin/env python3
"""
Patch Zalo to fix network state detection (gwig) and sync controller routing.
1. Fix gwig network state detection: CORS XMLHttpRequests to google.com cause getStateNetwork() to return DISCONNECT (0).
   Force getStateNetwork() to return u.CONNECTED, initialize stateCur to u.CONNECTED, and make _pingToDomain always resolve.
2. Route "Sync Messages" to SyncMessageController (V1) which sends push confirmation to mobile and uses db-cross-v4.
3. Remove premature NO_NETWORK throw (1106) in main-startup.
"""

import os
import sys
import subprocess

def patch_file(file_path, replacements):
    print(f"[*] Patching {file_path}...")
    if not os.path.exists(file_path):
        print(f"[-] File not found: {file_path}")
        return False

    with open(file_path, "r", encoding="utf-8") as f:
        content = f.read()

    orig_content = content
    for pattern, replacement in replacements:
        count = content.count(pattern)
        if count == 0:
            print(f"    [WARN] Pattern not found: {pattern[:60]}...")
        else:
            content = content.replace(pattern, replacement)
            print(f"    [OK] Replaced ({count}x): {pattern[:50]}...")

    if content == orig_content:
        print(f"    [INFO] No changes made to {file_path}")
        return True

    with open(file_path, "w", encoding="utf-8") as f:
        f.write(content)

    print(f"[+] Saved changes to {file_path}")
    return True

def main():
    base_dir = "/home/truongit/zalo-build/app"

    # 1. main-startup: bypass premature NO_NETWORK (1106) throw
    main_startup = os.path.join(base_dir, "pc-dist/lazy/main-startup.c2df40f39fa8f98b69a8.js")
    patch_file(main_startup, [
        ("ge.b.getStateNetwork()===ge.a.DISCONNECT||_g.default.getSocketState()!==mg.l.OPEN", "!1")
    ])

    # 2. UI bundles: fix gwig network state and route sync to SyncMessageController (V1)
    default_login = os.path.join(base_dir, "pc-dist/lazy/default-login-main-startup-shared-worker-znotification.2b8b4ccdfaf363018e90.js")
    patch_file(default_login, [
        ("const a=!0,s=!0,r=!0", "const a=!0,s=!1,r=!0"),
        ("getStateNetwork(){return this.stateCur}", "getStateNetwork(){return u.CONNECTED}"),
        ("_pingToDomain(e){return this._pingToDomainPC(e)}", "_pingToDomain(e){return Promise.resolve()}"),
        ("this.stateCur=u.NOT_SET", "this.stateCur=u.CONNECTED")
    ])

    other_bundles = [
        os.path.join(base_dir, "pc-dist/compact-app-pc.0d138e05abbfee05885d.js"),
        os.path.join(base_dir, "pc-dist/search-worker.0d138e05abbfee05885d.js"),
        os.path.join(base_dir, "pc-dist/sync-v2-sub-worker.0d138e05abbfee05885d.js")
    ]

    for ui_file in other_bundles:
        patch_file(ui_file, [
            ("const a=!0,i=!0,o=!0", "const a=!0,i=!1,o=!0"),
            ("getStateNetwork(){return this.stateCur}", "getStateNetwork(){return u.CONNECTED}"),
            ("_pingToDomain(e){return this._pingToDomainPC(e)}", "_pingToDomain(e){return Promise.resolve()}"),
            ("this.stateCur=u.NOT_SET", "this.stateCur=u.CONNECTED")
        ])

    print("\n[*] Validating JS syntax with node --check...")
    all_files = [main_startup, default_login] + other_bundles
    for f in all_files:
        res = subprocess.run(["node", "--check", f], capture_output=True, text=True)
        if res.returncode == 0:
            print(f"  [PASS] {os.path.basename(f)}")
        else:
            print(f"  [FAIL] {os.path.basename(f)}")
            print("STDERR:")
            print(res.stderr[:500])
            sys.exit(1)

    print("\n[+] All patches applied and verified!")

if __name__ == "__main__":
    main()
