#!/usr/bin/env python3
"""
Patch Zalo Call Signal Queue Deadlock:
Fix outgoing calls getting stuck at "Đang kết nối..." (Connecting...)
caused by Signal 401 (requestCall) and other call signals being pushed into
this.requestQueue which is never dequeued because gwig network state changes
were bypassed on Linux startup, leaving the internal queue state undefined.
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
            if replacement in content:
                print(f"    [INFO] Already patched: {pattern[:50]}...")
                continue
            print(f"    [WARN] Pattern not found: {repr(pattern[:60])}...")
            return False
        content = content.replace(pattern, replacement)
        print(f"    [OK] Replaced ({count}x): {pattern[:50]}... -> {replacement[:50]}...")

    if content == orig_content:
        print(f"    [INFO] No changes needed in {file_path}")
        return True

    # Save backup if not already present
    bak_path = file_path + ".bak-callfix"
    if not os.path.exists(bak_path):
        with open(bak_path, "w", encoding="utf-8") as f:
            f.write(orig_content)

    with open(file_path, "w", encoding="utf-8") as f:
        f.write(content)

    print(f"[+] Saved changes to {file_path}\n")
    return True

def apply_patches_to_base(base_dir):
    print(f"==================================================")
    print(f"Patching directory: {base_dir}")
    print(f"==================================================")

    # 1. compact-app-pc
    compact_app = os.path.join(base_dir, "pc-dist/compact-app-pc.0d138e05abbfee05885d.js")
    compact_replacements = [
        ("using_queue:1", "using_queue:0"),
        ("if(a)if(A.default.call.using_queue)", "if(a)if(!1&&A.default.call.using_queue)"),
        (
            "function P(e={}){const{limit:t=1/0,maxTimeout:n=L}=e,a=[];let i;const r=e=>{i=e}",
            "function P(e={}){const{limit:t=1/0,maxTimeout:n=L}=e,a=[];let i=w;const r=e=>{i=e}"
        ),
        (
            "this.requestQueue=P(),this.retryQueue=[]",
            "this.requestQueue=P(),this.requestQueue.dequeue(),this.retryQueue=[]"
        ),
        (
            "ged(e){h.n.listenEvent(h.i,(t=>{e(t)}))}logInfo(...e){}",
            'ged(e){h.n.listenEvent(h.i,(t=>{e(t)}))}logInfo(...e){try{console.log("[zcall-info]",...e)}catch(_){}}'
        )
    ]
    if not patch_file(compact_app, compact_replacements):
        return False

    # 2. default-login bundle
    default_login = os.path.join(base_dir, "pc-dist/lazy/default-login-main-startup-shared-worker-znotification.2b8b4ccdfaf363018e90.js")
    default_login_replacements = [
        ("using_queue:1", "using_queue:0"),
        ("if(a)if(A.default.call.using_queue)", "if(a)if(!1&&A.default.call.using_queue)"),
        (
            "function L(e={}){const{limit:t=1/0,maxTimeout:n=P}=e,a=[];let s;const i=e=>{s=e}",
            "function L(e={}){const{limit:t=1/0,maxTimeout:n=P}=e,a=[];let s=D;const i=e=>{s=e}"
        ),
        (
            "this.requestQueue=L(),this.retryQueue=[]",
            "this.requestQueue=L(),this.requestQueue.dequeue(),this.retryQueue=[]"
        ),
        (
            "ged(e){h.n.listenEvent(h.i,(t=>{e(t)}))}logInfo(...e){}",
            'ged(e){h.n.listenEvent(h.i,(t=>{e(t)}))}logInfo(...e){try{console.log("[zcall-info]",...e)}catch(_){}}'
        )
    ]
    if not patch_file(default_login, default_login_replacements):
        return False

    # 3. search-worker
    search_worker = os.path.join(base_dir, "pc-dist/search-worker.0d138e05abbfee05885d.js")
    search_replacements = [
        ("using_queue:1", "using_queue:0"),
        ("if(a)if(A.default.call.using_queue)", "if(a)if(!1&&A.default.call.using_queue)"),
        (
            "function L(e={}){const{limit:t=1/0,maxTimeout:n=P}=e,a=[];let i;const s=e=>{i=e}",
            "function L(e={}){const{limit:t=1/0,maxTimeout:n=P}=e,a=[];let i=w;const s=e=>{i=e}"
        ),
        (
            "this.requestQueue=L(),this.retryQueue=[]",
            "this.requestQueue=L(),this.requestQueue.dequeue(),this.retryQueue=[]"
        )
    ]
    if not patch_file(search_worker, search_replacements):
        return False

    # 4. sync-v2-sub-worker
    sync_worker = os.path.join(base_dir, "pc-dist/sync-v2-sub-worker.0d138e05abbfee05885d.js")
    sync_replacements = [
        ("using_queue:1", "using_queue:0"),
        ("if(a)if(A.default.call.using_queue)", "if(a)if(!1&&A.default.call.using_queue)"),
        (
            "function L(e={}){const{limit:t=1/0,maxTimeout:n=P}=e,a=[];let i;const s=e=>{i=e}",
            "function L(e={}){const{limit:t=1/0,maxTimeout:n=P}=e,a=[];let i=w;const s=e=>{i=e}"
        ),
        (
            "this.requestQueue=L(),this.retryQueue=[]",
            "this.requestQueue=L(),this.requestQueue.dequeue(),this.retryQueue=[]"
        )
    ]
    if not patch_file(sync_worker, sync_replacements):
        return False

    # Validate syntax with node --check
    print(f"[*] Validating JS syntax with node --check in {base_dir}...")
    for f in [compact_app, default_login, search_worker, sync_worker]:
        res = subprocess.run(["node", "--check", f], capture_output=True, text=True)
        if res.returncode == 0:
            print(f"  [PASS] {os.path.basename(f)}")
        else:
            print(f"  [FAIL] {os.path.basename(f)}")
            print("STDERR:")
            print(res.stderr[:500])
            return False

    return True

def main():
    root_dirs = [
        "/home/truongit/zalo-build/app",
        "/home/truongit/zalo-build/dist/Zalo.AppDir/app"
    ]

    for d in root_dirs:
        if not apply_patches_to_base(d):
            print(f"\n[-] Patching failed for {d}")
            sys.exit(1)

    print("\n[SUCCESS] Call signal queue deadlock patch applied and verified successfully!")

if __name__ == "__main__":
    main()
