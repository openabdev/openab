#!/usr/bin/env python3
"""
acp_surucu.py — agy-acp için headless stdio sürücüsü (Zed'siz test).

Taze derlenmiş `target/release/agy-acp`'i subprocess başlatır ve ACP 1.0
JSON-RPC akışını (newline-delimited JSON) stdio üzerinden sürer:
  initialize → session/new (cwd) → session/prompt (content blocks)
Agent'tan gelen session/request_permission isteklerini otomatik onaylar,
session/update bildirimlerini (mesaj/düşünce/araç) canlı yazar.

Kullanım:
    python3 test/acp_surucu.py "dizini listele ve README oku"
    python3 test/acp_surucu.py --cancel-after 8 "sleep 120"
    python3 test/acp_surucu.py --reddet "..."          # izin isteklerini REDDET
    python3 test/acp_surucu.py --timeout 180 "..."

Çıkış kodu: prompt stopReason 'end_turn'/'completed' ise 0, hata/timeout ise 1.
"""
import argparse
import base64
import json
import os
import subprocess
import sys
import threading
import time
from pathlib import Path

PROJE_KOK = Path(__file__).resolve().parent.parent
BINARY = PROJE_KOK / "target" / "release" / "agy-acp"

# ── Renkler (tty ise) ──────────────────────────────────────────────────────
_T = sys.stdout.isatty()
def _c(s, code): return f"\033[{code}m{s}\033[0m" if _T else s
def gri(s):  return _c(s, "90")
def mavi(s): return _c(s, "34")
def yes(s):  return _c(s, "32")
def sari(s): return _c(s, "33")
def kir(s):  return _c(s, "31")


class AcpSurucu:
    def __init__(self, proc, reddet=False, sessiz_dusunce=False):
        self.proc = proc
        self.reddet = reddet
        self.sessiz_dusunce = sessiz_dusunce
        self._yaz_kilit = threading.Lock()
        self._sonraki_id = 0
        self._bekleyen = {}          # id -> threading.Event
        self._yanitlar = {}          # id -> (result|error)
        self._kapali = False
        self.son_stop_reason = None
        self._okuyucu = threading.Thread(target=self._oku_dongu, daemon=True)
        self._okuyucu.start()

    # ── Düşük seviye G/Ç ────────────────────────────────────────────────
    def _gonder(self, obj):
        satir = json.dumps(obj, ensure_ascii=False)
        with self._yaz_kilit:
            self.proc.stdin.write(satir + "\n")
            self.proc.stdin.flush()

    def _istek(self, method, params, zaman_asimi=300):
        self._sonraki_id += 1
        rid = self._sonraki_id
        ev = threading.Event()
        self._bekleyen[rid] = ev
        self._gonder({"jsonrpc": "2.0", "id": rid, "method": method, "params": params})
        if not ev.wait(zaman_asimi):
            raise TimeoutError(f"{method} yanıtı {zaman_asimi}s içinde gelmedi")
        sonuc = self._yanitlar.pop(rid, None)
        if sonuc and "error" in sonuc:
            raise RuntimeError(f"{method} hatası: {sonuc['error']}")
        return sonuc.get("result") if sonuc else None

    def _bildir(self, method, params):
        self._gonder({"jsonrpc": "2.0", "method": method, "params": params})

    def _yanitla(self, rid, result=None, error=None):
        msg = {"jsonrpc": "2.0", "id": rid}
        if error is not None:
            msg["error"] = error
        else:
            msg["result"] = result if result is not None else {}
        self._gonder(msg)

    # ── Okuyucu döngüsü ────────────────────────────────────────────────
    def _oku_dongu(self):
        for ham in self.proc.stdout:
            ham = ham.strip()
            if not ham:
                continue
            try:
                msg = json.loads(ham)
            except json.JSONDecodeError:
                print(gri(f"  [parse edilemeyen satır] {ham[:200]}"))
                continue
            if "method" in msg and "id" in msg:
                self._gelen_istek(msg)          # agent→client isteği
            elif "method" in msg:
                self._gelen_bildirim(msg)        # agent→client notification
            elif "id" in msg:
                rid = msg["id"]
                self._yanitlar[rid] = msg        # bizim isteğimize yanıt
                ev = self._bekleyen.pop(rid, None)
                if ev:
                    ev.set()
        self._kapali = True
        for ev in self._bekleyen.values():
            ev.set()

    def _gelen_istek(self, msg):
        method = msg["method"]
        rid = msg["id"]
        if method == "session/request_permission":
            self._izin_yanitla(msg)
        elif method in ("fs/read_text_file", "fs/write_text_file"):
            self._yanitla(rid, result={})
        else:
            self._yanitla(rid, error={"code": -32601, "message": f"Desteklenmeyen: {method}"})

    def _izin_yanitla(self, msg):
        params = msg.get("params", {})
        secenekler = params.get("options", [])
        tc = params.get("toolCall", {})
        ad = tc.get("title") or tc.get("toolName") or "?"
        if self.reddet:
            tercih = next((o for o in secenekler if "reject" in o.get("kind", "")), None)
            etiket = kir("REDDET")
        else:
            tercih = next((o for o in secenekler if "allow" in o.get("kind", "")), None)
            etiket = yes("ONAY")
        if tercih is None and secenekler:
            tercih = secenekler[0]
        if tercih is None:
            self._yanitla(msg["id"], result={"outcome": {"outcome": "cancelled"}})
            return
        print(gri(f"  🔐 izin [{etiket}{gri(']')} {ad} → {tercih.get('name')}"))
        self._yanitla(msg["id"], result={
            "outcome": {"outcome": "selected", "optionId": tercih["optionId"]}
        })

    def _gelen_bildirim(self, msg):
        if msg["method"] != "session/update":
            return
        up = msg.get("params", {}).get("update", {})
        tip = up.get("sessionUpdate")
        if tip == "agent_message_chunk":
            metin = (up.get("content") or {}).get("text", "")
            if metin:
                sys.stdout.write(metin)
                sys.stdout.flush()
        elif tip == "agent_thought_chunk":
            if not self.sessiz_dusunce:
                metin = (up.get("content") or {}).get("text", "")
                if metin:
                    sys.stdout.write(gri(metin))
                    sys.stdout.flush()
        elif tip == "tool_call":
            ad = up.get("title") or up.get("toolName") or up.get("toolCallId", "?")
            print(mavi(f"\n  🔧 araç: {ad}  [{up.get('status', '?')}]"))
            self._arac_girdi(up)
        elif tip == "tool_call_update":
            durum = up.get("status")
            if durum:
                print(gri(f"     ↳ {up.get('toolCallId', '')} [{durum}]"))
            self._arac_girdi(up)
            self._arac_gozlem(up)
        elif tip == "plan":
            print(mavi(f"\n  📋 plan: {len(up.get('entries', []))} adım"))
        else:
            print(gri(f"\n  [update: {tip}]"))

    def _arac_girdi(self, up):
        ham = up.get("rawInput")
        if not ham:
            return
        try:
            s = json.dumps(ham, ensure_ascii=False)
        except Exception:
            s = str(ham)
        if len(s) > 2000:
            s = s[:2000] + f"…(+{len(s) - 2000} kr)"
        print(gri(f"     ⮑ girdi: {s}"))

    def _arac_gozlem(self, up):
        # hem single format hem list formatındaki content'leri desteklemek için
        contents = up.get("content", [])
        if isinstance(contents, dict):
            contents = [contents]
        for c in contents or []:
            if not isinstance(c, dict):
                continue
            tc = c.get("type")
            if tc == "content":
                metin = (c.get("content") or {}).get("text", "")
                if metin:
                    print(gri(f"     ⮑ gözlem: {metin}"))
            elif tc == "diff":
                print(gri(f"     ⮑ diff: {c.get('path', '')}"))

    # ── Yüksek seviye akış ─────────────────────────────────────────────
    def initialize(self):
        return self._istek("initialize", {
            "protocolVersion": 1,
            "clientCapabilities": {},
            "clientInfo": {"name": "acp_surucu", "version": "0.1.0"},
        }, zaman_asimi=60)

    def yeni_oturum(self, cwd):
        sonuc = self._istek("session/new", {
            "cwd": str(cwd),
            "mcpServers": [],
        }, zaman_asimi=120)
        return sonuc["sessionId"]

    def prompt(self, session_id, bloklar, zaman_asimi):
        sonuc = self._istek("session/prompt", {
            "sessionId": session_id,
            "prompt": bloklar,
        }, zaman_asimi=zaman_asimi)
        self.son_stop_reason = (sonuc or {}).get("stopReason")
        return self.son_stop_reason

    def iptal(self, session_id):
        print(sari(f"\n  ✖ session/cancel gönderiliyor (sessionId={session_id})"))
        self._bildir("session/cancel", {"sessionId": session_id})


def metin_blok(text):
    return {"type": "text", "text": text}


def resim_blok(yol):
    p = Path(yol)
    veri = base64.b64encode(p.read_bytes()).decode("ascii")
    ext = p.suffix.lower().lstrip(".")
    mime = {"jpg": "image/jpeg", "jpeg": "image/jpeg", "png": "image/png",
            "gif": "image/gif", "webp": "image/webp"}.get(ext, "image/png")
    return {"type": "image", "data": veri, "mimeType": mime}


def main():
    ap = argparse.ArgumentParser(description="agy-acp headless stdio sürücüsü")
    ap.add_argument("prompt", help="Ajana gönderilecek ilk istem metni")
    ap.add_argument("--then", action="append", default=[], dest="then",
                    help="Aynı oturumda sıralı gönderilecek ek istem (tekrarlanabilir)")
    ap.add_argument("--cwd", default=str(PROJE_KOK), help="Oturum çalışma dizini")
    ap.add_argument("--image", action="append", default=[], help="Eklenecek resim yolu")
    ap.add_argument("--cancel-after", type=float, default=None,
                    help="N saniye sonra session/cancel gönder")
    ap.add_argument("--timeout", type=float, default=300, help="prompt yanıt zaman aşımı (s)")
    ap.add_argument("--reddet", action="store_true", help="İzin isteklerini REDDET")
    ap.add_argument("--sessiz-dusunce", action="store_true", help="thinking chunk'larını gösterme")
    args = ap.parse_args()

    if not BINARY.exists():
        print(kir(f"Binary yok: {BINARY}\nÖnce: cargo build --release"), file=sys.stderr)
        return 2

    env = dict(os.environ)

    print(gri(f"→ başlatılıyor: {BINARY}"))
    print(gri(f"  cwd={args.cwd}  iptal={args.cancel_after}  reddet={args.reddet}"))
    proc = subprocess.Popen(
        [str(BINARY)],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=sys.stderr,
        env=env, text=True, bufsize=1,
    )

    surucu = AcpSurucu(proc, reddet=args.reddet, sessiz_dusunce=args.sessiz_dusunce)
    cikis = 1
    try:
        surucu.initialize()
        print(gri("✓ initialize"))
        sid = surucu.yeni_oturum(args.cwd)
        print(gri(f"✓ session/new → {sid}"))

        bloklar = [metin_blok(args.prompt)]
        for img in args.image:
            bloklar.append(resim_blok(img))

        if args.cancel_after is not None:
            def _gec_iptal():
                time.sleep(args.cancel_after)
                surucu.iptal(sid)
            threading.Thread(target=_gec_iptal, daemon=True).start()

        istemler = [(bloklar, args.prompt)]
        for t in args.then:
            istemler.append(([metin_blok(t)], t))

        cikis = 0
        for idx, (blk, metin) in enumerate(istemler):
            print(mavi(f"\n── PROMPT {idx+1}/{len(istemler)} ────────────────────────────\n{metin}\n──"))
            t0 = time.time()
            stop = surucu.prompt(sid, blk, zaman_asimi=args.timeout)
            sure = time.time() - t0
            print(f"\n\n{gri('──')} stopReason={yes(str(stop))}  ({sure:.1f}s)")
            if stop not in ("end_turn", "completed", "max_tokens", "max_turn_requests"):
                cikis = 1
                break
    except (TimeoutError, RuntimeError) as e:
        print(kir(f"\n✗ {e}"))
        cikis = 1
    except KeyboardInterrupt:
        print(sari("\n✖ kullanıcı kesti"))
        cikis = 130
    finally:
        try:
            proc.terminate()
            proc.wait(timeout=5)
        except Exception:
            proc.kill()
    return cikis


if __name__ == "__main__":
    sys.exit(main())
