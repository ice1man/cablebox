#!/usr/bin/env python3
"""Fake Tvheadend server for developing CableBox without a tuner.

Serves the subset of the Tvheadend HTTP API CableBox uses, with generated channels,
a 24h EPG, an in-memory DVR, ffmpeg test-pattern streams and generated logos.

    python3 tools/mock_tvh.py [--port 9981] [--user U --password P]

Then point CableBox at http://127.0.0.1:9981 (Login: Basic if --user is given).
"""

import argparse
import base64
import json
import os
import shutil
import subprocess
import tempfile
import threading
import time
import urllib.parse
import uuid as uuidlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

CHANNELS = [
    # (number, name, colour, tags)
    ("2", "KTV Local", "#c0392b", ["news"]),
    ("4", "Channel Four", "#2980b9", ["news"]),
    ("5.1", "PBS", "#27ae60", ["kids"]),
    ("5.2", "PBS Kids", "#f39c12", ["kids"]),
    ("7", "Sports Net", "#8e44ad", ["sports"]),
    ("9", "Movie Max", "#16a085", ["movies"]),
    ("11", "Weather Now", "#2c3e50", ["news"]),
    ("13", "Classic TV", "#d35400", []),
    ("22", "Cartoon Time", "#e84393", ["kids"]),
    ("31", "Science Ch", "#00b894", []),
    ("45", "Shopping", "#636e72", []),
    ("102", "Music Hits", "#6c5ce7", ["music"]),
]
TAGS = {"news": "News", "kids": "Kids", "sports": "Sports", "movies": "Movies", "music": "Music"}

SHOWS = {
    "news": [("Morning Report", 60), ("Midday News", 30), ("Evening News", 30), ("Late Edition", 60), ("Newsmakers", 30)],
    "kids": [("Puppet Pals", 30), ("Science Kids", 30), ("Cartoon Hour", 60), ("Story Time", 30)],
    "sports": [("SportsCenter", 60), ("Live: Football", 180), ("Highlights", 30), ("Fishing Weekly", 30)],
    "movies": [("The Long Night", 120), ("Space Rangers", 90), ("Romance in Paris", 120)],
    "music": [("Top 40 Countdown", 60), ("Unplugged", 30), ("Classic Videos", 60)],
    "": [("Rerun Theater", 30), ("Home Makeover", 60), ("Cooking Live", 30), ("Mystery Hour", 60), ("Game Show", 30)],
}

lock = threading.Lock()
channels = []
events = []  # dicts in Tvheadend EPG format
dvr = []  # dvr entries
autorecs = []
cache_dir = tempfile.mkdtemp(prefix="mock-tvh-")


def build():
    now = int(time.time())
    start0 = now - now % 1800 - 3 * 3600
    eid = 1000
    for i, (num, name, colour, tags) in enumerate(CHANNELS):
        cu = f"ch{i:02d}" + uuidlib.uuid5(uuidlib.NAMESPACE_DNS, name).hex[:10]
        channels.append({
            "uuid": cu, "enabled": True, "name": name,
            "number": float(num) if "." in num else int(num),
            "icon_public_url": f"imagecache/{i + 1}", "tags": [f"tag-{t}" for t in tags],
            "_colour": colour,
        })
        pool = SHOWS[tags[0] if tags else ""]
        t = start0
        k = i
        while t < now + 26 * 3600:
            title, mins = pool[k % len(pool)]
            k += 1
            ep = k % 20 + 1
            events.append({
                "eventId": eid, "channelUuid": cu, "channelName": name, "channelNumber": num,
                "start": t, "stop": t + mins * 60, "title": title,
                "subtitle": f"Episode {ep}" if mins <= 60 else None,
                "description": f"{title} on {name}. A thrilling {mins}-minute program full of surprises.",
                "episodeOnscreen": f"S0{ep % 4 + 1}E{ep:02d}" if mins <= 60 else None,
                "serieslinkUri": f"crid://{cu}/{title}" if mins <= 60 else None,
            })
            eid += 1
            t += mins * 60
    # One finished recording to play back.
    first = events[5]
    dvr.append(dvr_entry(first, "completed", "Completed OK", filesize=734003200))


def dvr_entry(ev, sched, status, filesize=0):
    return {
        "uuid": uuidlib.uuid4().hex, "disp_title": ev["title"], "disp_subtitle": ev.get("subtitle"),
        "disp_description": ev.get("description"), "channel": ev["channelUuid"],
        "channelname": ev["channelName"], "start": ev["start"], "stop": ev["stop"],
        "sched_status": sched, "status": status, "filesize": filesize, "broadcast": ev["eventId"],
    }


def dvr_state():
    now = int(time.time())
    for d in dvr:
        if d["sched_status"] in ("scheduled", "recording"):
            if d["stop"] <= now:
                d["sched_status"], d["status"], d["filesize"] = "completed", "Completed OK", 524288000
            elif d["start"] <= now:
                d["sched_status"], d["status"] = "recording", "Running"


def grid(entries):
    return {"entries": entries, "total": len(entries)}


def logo_png(i):
    path = os.path.join(cache_dir, f"logo{i}.png")
    if not os.path.exists(path):
        ch = channels[i - 1]
        subprocess.run(
            ["ffmpeg", "-loglevel", "error", "-f", "lavfi", "-i", f"color=c={ch['_colour']}:s=128x64",
             "-vf", f"drawtext=text='{ch['name'][:3].upper()}':fontcolor=white:fontsize=34:x=(w-tw)/2:y=(h-th)/2",
             "-frames:v", "1", "-y", path],
            check=False,
        )
    return path


def recording_file():
    path = os.path.join(cache_dir, "recording.ts")
    if not os.path.exists(path):
        subprocess.run(
            ["ffmpeg", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc2=size=1280x720:rate=30:d=60",
             "-f", "lavfi", "-i", "sine=frequency=330:d=60", "-c:v", "libx264", "-preset", "ultrafast",
             "-c:a", "aac", "-f", "mpegts", "-y", path],
            check=False,
        )
    return path


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    auth = None

    def log_message(self, fmt, *args):
        if os.environ.get("MOCK_TVH_VERBOSE"):
            super().log_message(fmt, *args)

    def authorized(self):
        if not Handler.auth:
            return True
        hdr = self.headers.get("Authorization", "")
        if hdr.startswith("Basic ") and base64.b64decode(hdr[6:]).decode() == Handler.auth:
            return True
        self.send_response(401)
        self.send_header("WWW-Authenticate", 'Basic realm="tvheadend"')
        self.send_header("Content-Length", "0")
        self.end_headers()
        return False

    def params(self):
        url = urllib.parse.urlparse(self.path)
        q = dict(urllib.parse.parse_qsl(url.query))
        if self.command == "POST":
            n = int(self.headers.get("Content-Length") or 0)
            q.update(urllib.parse.parse_qsl(self.rfile.read(n).decode()))
        return url.path, q

    def send_json(self, obj):
        body = json.dumps(obj).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def send_file(self, path, ctype):
        with open(path, "rb") as f:
            data = f.read()
        self.send_response(200)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        self.route()

    def do_POST(self):
        self.route()

    def route(self):
        path, q = self.params()
        if not self.authorized():
            return
        with lock:
            dvr_state()
            if path == "/api/serverinfo":
                return self.send_json({"sw_version": "4.3-mock", "api_version": 19, "name": "Mock Tvheadend"})
            if path == "/api/channel/grid":
                return self.send_json(grid([{k: v for k, v in c.items() if not k.startswith("_")} for c in channels]))
            if path == "/api/channeltag/grid":
                return self.send_json(grid([{"uuid": f"tag-{k}", "name": v, "enabled": True, "internal": False, "index": i}
                                            for i, (k, v) in enumerate(TAGS.items())]))
            if path == "/api/epg/events/grid":
                lo, hi = -2**62, 2**62
                for f in json.loads(q.get("filter", "[]")):
                    if f["field"] == "stop" and f["comparison"] == "gt":
                        lo = f["value"]
                    if f["field"] == "start" and f["comparison"] == "lt":
                        hi = f["value"]
                out = []
                for e in events:
                    if e["stop"] > lo and e["start"] < hi and q.get("channel") in (None, e["channelUuid"]):
                        d = next((d for d in dvr if d["broadcast"] == e["eventId"]
                                  and d["sched_status"] in ("scheduled", "recording")), None)
                        out.append(dict(e, dvrUuid=d["uuid"], dvrState=d["sched_status"]) if d else e)
                return self.send_json({"entries": out, "totalCount": len(out)})
            if path == "/api/dvr/entry/grid_upcoming":
                return self.send_json(grid(sorted([d for d in dvr if d["sched_status"] in ("scheduled", "recording")],
                                                  key=lambda d: d["start"])))
            if path == "/api/dvr/entry/grid_finished":
                return self.send_json(grid([d for d in dvr if d["sched_status"] == "completed"]))
            if path == "/api/dvr/autorec/grid":
                return self.send_json(grid(autorecs))
            if path in ("/api/dvr/entry/create_by_event", "/api/dvr/autorec/create_by_series"):
                ev = next((e for e in events if e["eventId"] == int(q["event_id"])), None)
                if not ev:
                    return self.send_json({})
                if path.endswith("create_by_series"):
                    autorecs.append({"uuid": uuidlib.uuid4().hex, "name": ev["title"], "title": ev["title"],
                                     "enabled": True, "channel": ev["channelUuid"]})
                    eps = [e for e in events if e["title"] == ev["title"] and e["channelUuid"] == ev["channelUuid"]
                           and e["start"] >= ev["start"]]
                else:
                    eps = [ev]
                for e in eps:
                    if not any(d["broadcast"] == e["eventId"] for d in dvr):
                        dvr.append(dvr_entry(e, "scheduled", "Scheduled for recording"))
                return self.send_json({})
            if path in ("/api/dvr/entry/cancel", "/api/dvr/entry/remove", "/api/idnode/delete"):
                u = q.get("uuid")
                dvr[:] = [d for d in dvr if d["uuid"] != u]
                autorecs[:] = [a for a in autorecs if a["uuid"] != u]
                return self.send_json({})
            if path.startswith("/imagecache/"):
                i = int(path.rsplit("/", 1)[1])
                if 1 <= i <= len(channels):
                    return self.send_file(logo_png(i), "image/png")
            if path.startswith("/dvrfile/"):
                return self.send_file(recording_file(), "video/mp2t")
            if path.startswith("/stream/channel/"):
                ch = next((c for c in channels if c["uuid"] == path.rsplit("/", 1)[1]), None)
            else:
                ch = None
        if path.startswith("/stream/channel/") and ch:
            return self.stream(ch)
        self.send_response(404)
        self.send_header("Content-Length", "0")
        self.end_headers()

    def stream(self, ch):
        """Endless MPEG-TS test pattern tinted with the channel's colour."""
        label = ch["name"].replace("'", "")
        vf = (f"drawbox=x=0:y=ih-90:w=iw:h=90:color={ch['_colour']}@0.85:t=fill,"
              f"drawtext=text='{label}':fontcolor=white:fontsize=48:x=40:y=h-75,"
              "drawtext=text='%{localtime\\:%H\\\\\\:%M\\\\\\:%S}':fontcolor=white:fontsize=40:x=w-tw-40:y=h-70")
        cmd = ["ffmpeg", "-loglevel", "error", "-re",
               "-f", "lavfi", "-i", "testsrc2=size=1280x720:rate=30",
               "-f", "lavfi", "-i", f"sine=frequency={220 + 40 * channels.index(ch)}",
               "-vf", vf, "-c:v", "libx264", "-preset", "ultrafast", "-tune", "zerolatency", "-g", "30",
               "-c:a", "aac", "-f", "mpegts", "pipe:1"]
        proc = subprocess.Popen(cmd, stdout=subprocess.PIPE)
        self.send_response(200)
        self.send_header("Content-Type", "video/mp2t")
        self.send_header("Connection", "close")
        self.end_headers()
        try:
            while chunk := proc.stdout.read(65536):
                self.wfile.write(chunk)
        except (BrokenPipeError, ConnectionResetError):
            pass
        finally:
            proc.kill()
            proc.wait()
            self.close_connection = True


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--port", type=int, default=9981)
    ap.add_argument("--bind", default="127.0.0.1")
    ap.add_argument("--user")
    ap.add_argument("--password", default="")
    a = ap.parse_args()
    if not shutil.which("ffmpeg"):
        raise SystemExit("ffmpeg is required for streams and logos")
    if a.user:
        Handler.auth = f"{a.user}:{a.password}"
    build()
    print(f"mock tvheadend on http://{a.bind}:{a.port} — {len(channels)} channels, {len(events)} EPG events")
    ThreadingHTTPServer((a.bind, a.port), Handler).serve_forever()


if __name__ == "__main__":
    main()
