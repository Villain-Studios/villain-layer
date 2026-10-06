import { useCallback, useEffect, useRef, useState } from "react";
import { follow, phoneApi, token, Unpaired } from "./client";
import { Overview } from "./Overview";
import { PaneView } from "./PaneView";
import { Pair } from "./Pair";
import type { Overview as Data } from "./types";

/** The pane open, if one is: `#pane=<id>`, so Back on the phone goes back. */
function paneInHash(): string | null {
  const m = /^#pane=(.+)$/.exec(location.hash);
  return m ? decodeURIComponent(m[1]) : null;
}

export function App() {
  const [paired, setPaired] = useState(() => token() !== null);
  const [data, setData] = useState<Data | null>(null);
  const [live, setLive] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [pane, setPane] = useState(paneInHash);

  useEffect(() => {
    const onToken = () => setPaired(token() !== null);
    const onHash = () => setPane(paneInHash());
    window.addEventListener("villain-token", onToken);
    window.addEventListener("hashchange", onHash);
    return () => {
      window.removeEventListener("villain-token", onToken);
      window.removeEventListener("hashchange", onHash);
    };
  }, []);

  // One look at a time; a change during one asks for one more after it.
  const loading = useRef(false);
  const again = useRef(false);
  const load = useCallback(async () => {
    if (loading.current) {
      again.current = true;
      return;
    }
    loading.current = true;
    try {
      do {
        again.current = false;
        setData(await phoneApi.overview());
        setError(null);
      } while (again.current);
    } catch (e) {
      if (!(e instanceof Unpaired)) setError(e instanceof Error ? e.message : String(e));
    } finally {
      loading.current = false;
    }
  }, []);

  // Told when anything changes, rather than asking on a timer.
  useEffect(() => {
    if (!paired) return;
    return follow(
      () => "/api/changes",
      () => void load(),
      (up) => {
        setLive(up);
        // Whatever changed while the stream was down.
        if (up) void load();
      },
    );
  }, [paired, load]);

  if (!paired) return <Pair />;
  const open = pane ? data?.groups.flatMap((g) => g.panes).find((p) => p.id === pane) : undefined;
  if (pane) {
    return (
      <PaneView
        id={pane}
        pane={open ?? null}
        typing={data?.typing ?? false}
        onBack={() => history.length > 1 ? history.back() : (location.hash = "")}
      />
    );
  }
  return <Overview data={data} live={live} error={error} onOpen={(id) => (location.hash = `pane=${encodeURIComponent(id)}`)} />;
}
