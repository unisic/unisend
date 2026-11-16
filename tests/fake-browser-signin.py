#!/usr/bin/env python3
"""Fake homeserver in front of the local Synapse, for testing browser sign-in.

fake-hs.py PORT oauth|sso USER [EXPIRES]
oauth: next-gen auth (MSC3861) with dynamic registration, PKCE S256, refresh tokens that rotate.
       Access tokens are fake, expire after EXPIRES seconds (M_UNKNOWN_TOKEN soft_logout), and map
       to a real Synapse token got by JWT login for USER with the requested device ID.
sso:   m.login.sso with one identity provider; m.login.token becomes a JWT login for USER.
Everything else is proxied to Synapse. The authorize and SSO redirect pages answer at once with a
302 back to the client, as a browser where the user already approved would.
"""
import base64, hashlib, hmac, json, secrets, sys, threading, time, urllib.parse, urllib.request, urllib.error
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT, MODE, USER = int(sys.argv[1]), sys.argv[2], sys.argv[3]
EXPIRES = int(sys.argv[4]) if len(sys.argv) > 4 else 30
SYNAPSE = "http://127.0.0.1:8008"
SECRET = b"test-secret-not-for-production"
BASE = f"http://127.0.0.1:{PORT}/"
lock = threading.Lock()
clients, codes, access, refresh = {}, {}, {}, {}
stats = {"refreshed": 0, "expired": 0, "registered": 0, "logins": 0}


def b64(b):
    return base64.urlsafe_b64encode(b).rstrip(b"=").decode()


def jwt(sub):
    head = b64(b'{"alg":"HS256","typ":"JWT"}') + "." + b64(json.dumps({"sub": sub}).encode())
    return head + "." + b64(hmac.new(SECRET, head.encode(), hashlib.sha256).digest())


def synapse_login(device):
    body = {"type": "org.matrix.login.jwt", "token": jwt(USER), "initial_device_display_name": "Unisend"}
    if device:
        body["device_id"] = device
    req = urllib.request.Request(SYNAPSE + "/_matrix/client/v3/login", json.dumps(body).encode(), {"Content-Type": "application/json"})
    return json.load(urllib.request.urlopen(req, timeout=30))


def issue(real, device):
    tok, ref = "fake_at_" + secrets.token_hex(12), "fake_rt_" + secrets.token_hex(12)
    access[tok] = (real, time.time() + EXPIRES)
    refresh[ref] = (real, device)
    return {"access_token": tok, "token_type": "Bearer", "expires_in": EXPIRES, "refresh_token": ref,
            "scope": f"urn:matrix:client:api:* urn:matrix:client:device:{device}"}


class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *a):
        sys.stderr.write("%s %s\n" % (self.command, self.path.split("?")[0]))

    def send(self, code, body=b"", headers=()):
        if isinstance(body, (dict, list)):
            body = json.dumps(body).encode()
            headers = list(headers) + [("Content-Type", "application/json")]
        self.send_response(code)
        for k, v in headers:
            self.send_header(k, v)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def body(self):
        n = int(self.headers.get("Content-Length") or 0)
        return self.rfile.read(n) if n else b""

    def route(self):
        u = urllib.parse.urlsplit(self.path)
        q = dict(urllib.parse.parse_qsl(u.query))
        p = u.path
        if p == "/_matrix/client/v1/auth_metadata" or p == "/_matrix/client/unstable/org.matrix.msc2965/auth_metadata":
            if MODE != "oauth":
                return self.send(404, {"errcode": "M_UNRECOGNIZED", "error": "Unrecognized request"})
            return self.send(200, {
                "issuer": BASE, "authorization_endpoint": BASE + "authorize", "token_endpoint": BASE + "token",
                "registration_endpoint": BASE + "register", "revocation_endpoint": BASE + "revoke",
                "response_types_supported": ["code"], "response_modes_supported": ["query", "fragment"],
                "grant_types_supported": ["authorization_code", "refresh_token"],
                "code_challenge_methods_supported": ["S256"], "prompt_values_supported": ["create"]})
        if p == "/_matrix/client/v3/login" and self.command == "GET":
            if MODE == "oauth":
                return self.send(200, {"flows": [{"type": "m.login.sso", "oauth_aware_preferred": True}, {"type": "m.login.token"}]})
            return self.send(200, {"flows": [{"type": "m.login.sso", "identity_providers": [{"id": "fake", "name": "Fake IdP"}]}, {"type": "m.login.token"}]})
        if p.startswith("/_matrix/client/v3/login/sso/redirect"):
            back = q["redirectUrl"]
            tok = "lt_" + secrets.token_hex(8)
            with lock:
                codes[tok] = True
            sep = "&" if "?" in back else "?"
            return self.send(302, headers=[("Location", back + sep + "loginToken=" + tok)])
        if p == "/_matrix/client/v3/login" and self.command == "POST":
            b = json.loads(self.body() or b"{}")
            if b.get("type") == "m.login.token":
                with lock:
                    ok = codes.pop(b.get("token"), None)
                if not ok:
                    return self.send(403, {"errcode": "M_FORBIDDEN", "error": "bad login token"})
                stats["logins"] += 1
                return self.send(200, synapse_login(b.get("device_id")))
            return self.proxy(json.dumps(b).encode())
        if p == "/register" and self.command == "POST":
            meta = json.loads(self.body())
            cid = "client_" + secrets.token_hex(6)
            with lock:
                clients[cid] = meta
            stats["registered"] += 1
            sys.stderr.write("registered %s\n" % json.dumps({k: meta.get(k) for k in ("application_type", "client_name", "client_uri", "redirect_uris", "grant_types", "token_endpoint_auth_method")}))
            return self.send(201, {"client_id": cid, "client_id_issued_at": int(time.time())})
        if p == "/authorize":
            meta = clients.get(q.get("client_id"))
            if not meta or q.get("redirect_uri") not in meta.get("redirect_uris", []) or q.get("response_type") != "code" \
                    or q.get("code_challenge_method") != "S256" or not q.get("state"):
                return self.send(400, b"bad authorization request")
            device = next((s.rsplit(":", 1)[1] for s in q.get("scope", "").split() if s.startswith("urn:matrix:client:device:") or s.startswith("urn:matrix:org.matrix.msc2967.client:device:")), None)
            code = "code_" + secrets.token_hex(8)
            with lock:
                codes[code] = (q["code_challenge"], device, q["redirect_uri"], q["client_id"])
            return self.send(302, headers=[("Location", q["redirect_uri"] + "?" + urllib.parse.urlencode({"code": code, "state": q["state"]}))])
        if p == "/token" and self.command == "POST":
            f = dict(urllib.parse.parse_qsl(self.body().decode()))
            if f.get("grant_type") == "authorization_code":
                with lock:
                    c = codes.pop(f.get("code"), None)
                if not c or b64(hashlib.sha256(f.get("code_verifier", "").encode()).digest()) != c[0] or f.get("redirect_uri") != c[2]:
                    return self.send(400, {"error": "invalid_grant"})
                real = synapse_login(c[1])
                stats["logins"] += 1
                with lock:
                    return self.send(200, issue(real["access_token"], real["device_id"]))
            if f.get("grant_type") == "refresh_token":
                with lock:
                    r = refresh.pop(f.get("refresh_token"), None)
                    if not r:
                        return self.send(400, {"error": "invalid_grant"})
                    stats["refreshed"] += 1
                    sys.stderr.write("refreshed %d\n" % stats["refreshed"])
                    return self.send(200, issue(*r))
            return self.send(400, {"error": "unsupported_grant_type"})
        if p == "/revoke":
            return self.send(200, b"")
        if p == "/stats":
            return self.send(200, stats)
        return self.proxy(self.body())

    def proxy(self, data):
        headers = {k.title(): v for k, v in self.headers.items() if k.lower() not in ("host", "content-length", "connection", "accept-encoding")}
        auth = headers.get("Authorization", "")
        if auth.startswith("Bearer fake_at_"):
            with lock:
                hit = access.get(auth[7:])
            if not hit or hit[1] < time.time():
                stats["expired"] += 1
                return self.send(401, {"errcode": "M_UNKNOWN_TOKEN", "error": "Access token has expired", "soft_logout": True})
            headers["Authorization"] = "Bearer " + hit[0]
        req = urllib.request.Request(SYNAPSE + self.path, data if self.command in ("POST", "PUT") else None, headers, method=self.command)
        try:
            r = urllib.request.urlopen(req, timeout=90)
            code, body, rh = r.status, r.read(), r.headers
        except urllib.error.HTTPError as e:
            code, body, rh = e.code, e.read(), e.headers
        self.send(code, body, [(k, v) for k, v in rh.items() if k.lower() == "content-type"])

    do_GET = do_POST = do_PUT = do_DELETE = route


ThreadingHTTPServer(("127.0.0.1", PORT), H).serve_forever()
