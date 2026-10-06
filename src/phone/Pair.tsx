import { useState } from "react";
import { phoneApi } from "./client";

/** What this phone calls itself on the Mac's list, until renamed there. */
function guessName(): string {
  const ua = navigator.userAgent;
  if (/iPad/.test(ua)) return "iPad";
  if (/iPhone/.test(ua)) return "iPhone";
  if (/Android/.test(ua)) return "Android phone";
  return "Phone";
}

/** Pair this phone with the code showing in the app's Settings (PHONE-3). */
export function Pair() {
  const [code, setCode] = useState("");
  const [name, setName] = useState(guessName);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function pair() {
    setBusy(true);
    setError(null);
    try {
      await phoneApi.pair(code.replace(/\D/g, ""), name);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  const ready = code.replace(/\D/g, "").length === 6 && !busy;
  return (
    <div className="pair">
      <div className="brand">villain<span>·</span>layer</div>
      <p>
        On the Mac, open the app's <b>Settings</b>, then <b>Phone</b>, and choose <b>Pair a phone</b>. Type the code
        it shows.
      </p>
      <form
        onSubmit={(e) => {
          e.preventDefault();
          if (ready) void pair();
        }}
      >
        <input
          className="code"
          inputMode="numeric"
          autoComplete="one-time-code"
          placeholder="123 456"
          maxLength={7}
          value={code}
          onChange={(e) => setCode(e.target.value)}
          autoFocus
        />
        <label>
          This phone's name
          <input value={name} maxLength={40} onChange={(e) => setName(e.target.value)} />
        </label>
        <button className="primary" disabled={!ready}>
          {busy ? "Pairing…" : "Pair"}
        </button>
      </form>
      {error && <div className="error">{error}</div>}
    </div>
  );
}
