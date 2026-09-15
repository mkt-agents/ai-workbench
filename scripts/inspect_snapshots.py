import json, os, base64, time
from pathlib import Path

base = Path(os.environ["APPDATA"]) / "com.ai-workbench.app" / "cursor-backups"
now = int(time.time())


def decode_jwt(t: str):
    try:
        p = t.split(".")[1]
        p += "=" * ((4 - len(p) % 4) % 4)
        return json.loads(base64.urlsafe_b64decode(p))
    except Exception as e:
        return {"error": str(e)}


for d in sorted(base.iterdir()):
    auth = d / "auth.json"
    cookies = d / "Network" / "Cookies"
    if not auth.exists():
        continue
    data = json.loads(auth.read_text(encoding="utf-8"))
    email = data.get("email", "")
    items = data.get("items", {})
    tok = items.get("cursorAuth/accessToken", "")
    ref = items.get("cursorAuth/refreshToken", "")
    aj = decode_jwt(tok) if tok else {}
    print("===", d.name, email)
    print("  cookies", cookies.exists(), cookies.stat().st_size if cookies.exists() else 0)
    print("  access==refresh", tok == ref)
    print("  jwt sub", aj.get("sub"))
    print("  jwt exp", aj.get("exp"), "left", (aj.get("exp") or 0) - now if aj.get("exp") else None)
    print("  jwt time", aj.get("time"))
    print("  key count", len(items))
