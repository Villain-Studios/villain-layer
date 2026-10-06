import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api } from "../lib/api";
import { useStore } from "../store";
import type { PhoneStatus } from "../lib/types";
import { PhoneIcon } from "./icons";
import { Field, Switch } from "./ui";

/** Phone access as the backend has it, kept current by `phone:changed`. */
function usePhoneStatus(): [PhoneStatus | null, (s: PhoneStatus) => void, () => void] {
  const [status, setStatus] = useState<PhoneStatus | null>(null);
  const [asked, setAsked] = useState(0);
  useEffect(() => {
    let current = true;
    const load = () => {
      api.phoneStatus().then((s) => { if (current) setStatus(s); }).catch(() => {});
    };
    load();
    const p = listen("phone:changed", load);
    return () => {
      current = false;
      void p.then((un) => un());
    };
  }, [asked]);
  return [status, setStatus, () => setAsked((n) => n + 1)];
}

const WAY = { tailscale: "Tailscale", home: "Home network" } as const;

/** Settings → Phone (PHONE-1..8). */
export function PhoneSettings() {
  const [status, setStatus, reload] = usePhoneStatus();
  const refreshSettings = useStore((s) => s.refreshSettings);
  const fail = useStore((s) => s.fail);
  const [busy, setBusy] = useState(false);

  async function save(patch: Partial<Pick<PhoneStatus, "tailscale" | "home" | "typing">>) {
    if (!status) return;
    const next = { ...status, ...patch };
    setBusy(true);
    try {
      setStatus(await api.setPhoneAccess(next.tailscale, next.home, next.typing));
      // Keep awake reads whether a way is open (PHONE-8).
      await refreshSettings();
    } catch (e) {
      fail(e);
    } finally {
      setBusy(false);
    }
  }

  async function pair() {
    try {
      const pairing = await api.phonePair();
      if (status) setStatus({ ...status, pairing });
    } catch (e) {
      fail(e);
    }
  }

  async function forget(id: string) {
    try {
      await api.phoneForget(id);
      reload();
    } catch (e) {
      fail(e);
    }
  }

  if (!status) return null;
  const open = status.tailscale || status.home;
  return (
    <>
      <Field
        label="Ways in"
        hint="Everything keeps running on this Mac; a phone sees the tasks and their agents, and follows a terminal. The app answers on the network only while one of these is on, and only to callers on that way."
      >
        <div className="switch-list">
          <Switch
            label="Tailscale"
            detail="From anywhere, over your tailnet. Install Tailscale on this Mac and on the phone, signed in to the same account."
            checked={status.tailscale}
            disabled={busy}
            onChange={(v) => void save({ tailscale: v })}
          />
          <Switch
            label="Home network and router VPN"
            detail="On the same Wi-Fi as this Mac, or from anywhere through your router's own VPN (WireGuard on a GL.iNet router, say), which puts the phone on the home network."
            checked={status.home}
            disabled={busy}
            onChange={(v) => void save({ home: v })}
          />
        </div>
      </Field>

      {status.error && <div className="phone-error">{status.error}</div>}

      {open && status.listening && (
        <Field
          label="Open on the phone"
          hint="Then Share → Add to Home Screen, so it opens like an app. Closing the lid still puts the Mac to sleep."
        >
          {status.addresses.length === 0 ? (
            <div className="phone-hint">
              This Mac has no address on {status.tailscale && !status.home ? "Tailscale" : "these ways"} right now.
              {status.tailscale && " Is Tailscale running and signed in?"}
            </div>
          ) : (
            <div className="phone-list">
              {status.addresses.map((a) => (
                <div key={a.ip} className="row">
                  <span className="chip">{WAY[a.way]}</span>
                  <code>http://{a.ip}:{status.port}</code>
                  <span className="phone-hint">{a.interface}</span>
                </div>
              ))}
            </div>
          )}
        </Field>
      )}

      <Field label="Typing">
        <div className="switch-list">
          <Switch
            label="Let phones type into agents"
            detail="A line of text, and the keys an agent's questions take: Enter, Esc, the arrows, 1 to 3, Ctrl-C. Into agents only, never shells. Off, a phone only watches."
            checked={status.typing}
            disabled={busy}
            onChange={(v) => void save({ typing: v })}
          />
        </div>
      </Field>

      <Field
        label="Phones"
        hint="A phone gets in only with a code shown here, and each paired phone has a token of its own. Forget one and it is out at once."
      >
        {status.pairing ? (
          <Pairing code={status.pairing.code} seconds={status.pairing.seconds_left} onExpired={reload} />
        ) : (
          <button className="btn" disabled={!status.listening} onClick={() => void pair()}>
            Pair a phone
          </button>
        )}
        {status.devices.length > 0 && (
          <div className="phone-list">
            {status.devices.map((d) => (
              <div key={d.id} className="row">
                <span className={`dot ${d.connected ? "live" : ""}`} title={d.connected ? "Connected now" : "Not connected"} />
                <span>{d.name}</span>
                <span className="phone-hint">paired {new Date(d.paired_at).toLocaleDateString()}</span>
                <div className="spacer" />
                <button className="btn btn-sm" onClick={() => void forget(d.id)}>Forget</button>
              </div>
            ))}
          </div>
        )}
      </Field>
    </>
  );
}

/** The code to type on the phone, and how long it has left. */
function Pairing({ code, seconds, onExpired }: { code: string; seconds: number; onExpired: () => void }) {
  const [left, setLeft] = useState(seconds);
  useEffect(() => {
    setLeft(seconds);
    // guard: allow poll — a countdown on screen while a code shows; nothing changes in the backend to push.
    const t = window.setInterval(() => setLeft((n) => Math.max(0, n - 1)), 1000);
    return () => window.clearInterval(t);
  }, [code, seconds]);
  useEffect(() => {
    if (left === 0) onExpired();
  }, [left, onExpired]);
  return (
    <div className="phone-pairing">
      <div className="phone-code">{code.slice(0, 3)} {code.slice(3)}</div>
      <div className="phone-hint">
        Open the address above on the phone and type this code. It works once, for {Math.floor(left / 60)}:
        {String(left % 60).padStart(2, "0")} more.
      </div>
    </div>
  );
}

/** In the top bar while a paired phone has the app open. */
export function PhoneBadge() {
  const [status] = usePhoneStatus();
  const toggleSettings = useStore((s) => s.toggleSettings);
  const connected = status?.devices.filter((d) => d.connected) ?? [];
  if (connected.length === 0) return null;
  return (
    <button
      className="icon-btn phone-on"
      title={`${connected.map((d) => d.name).join(", ")} ${connected.length === 1 ? "has" : "have"} the app open`}
      onClick={() => toggleSettings(true)}
    >
      <PhoneIcon />
    </button>
  );
}
