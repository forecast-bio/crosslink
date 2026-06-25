// xterm.js-backed terminal view for a PTY session.
//
// Mounts an xterm Terminal, wires it to the broker WebSocket at
// `/ws/pty/<session_id>`, and ferries bytes both ways:
//   - Server frames `{type:"stdout", data: <base64>}` → terminal.write
//   - Server frames `{type:"exit", code}`             → render banner
//   - User keystrokes                                  → `{type:"stdin", data}`
//   - Browser resize                                   → `{type:"resize", rows, cols}`
//
// The replay buffer the server sends on connect lets a fresh tab
// re-render the most recent ~64 KiB of output instead of starting
// blank — useful when reconnecting to an in-flight session.

import { useEffect, useRef, useState } from "react";
import { Terminal } from "xterm";
import { FitAddon } from "xterm-addon-fit";
import "xterm/css/xterm.css";

interface ServerStdout {
  type: "stdout";
  data: string;
}

interface ServerExit {
  type: "exit";
  code: number | null;
}

type ServerFrame = ServerStdout | ServerExit;

function toB64(bytes: Uint8Array): string {
  let s = "";
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s);
}

function fromB64(data: string): Uint8Array {
  const bin = atob(data);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i += 1) out[i] = bin.charCodeAt(i);
  return out;
}

export function PtyTerminal({
  sessionId,
  onExit,
}: {
  sessionId: string;
  onExit?: (code: number | null) => void;
}) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const wsRef = useRef<WebSocket | null>(null);
  const [status, setStatus] = useState<"connecting" | "open" | "closed">(
    "connecting",
  );
  const [exitInfo, setExitInfo] = useState<number | null | "open">("open");

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    const term = new Terminal({
      cursorBlink: true,
      convertEol: true,
      fontFamily:
        "ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace",
      fontSize: 13,
      theme: { background: "#0a0a0a" },
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(container);
    fit.fit();
    termRef.current = term;
    fitRef.current = fit;

    // Same-origin ws:// (or wss:// when served over TLS).
    const proto = window.location.protocol === "https:" ? "wss:" : "ws:";
    const ws = new WebSocket(`${proto}//${window.location.host}/ws/pty/${sessionId}`);
    ws.binaryType = "arraybuffer";
    wsRef.current = ws;

    ws.onopen = () => {
      setStatus("open");
      // Tell the server our actual terminal size.
      ws.send(
        JSON.stringify({
          type: "resize",
          rows: term.rows,
          cols: term.cols,
        }),
      );
    };
    ws.onclose = () => {
      setStatus("closed");
    };
    ws.onerror = () => {
      term.write("\r\n[connection error]\r\n");
    };
    ws.onmessage = (ev) => {
      let frame: ServerFrame;
      try {
        frame = JSON.parse(typeof ev.data === "string" ? ev.data : "") as ServerFrame;
      } catch {
        return;
      }
      if (frame.type === "stdout") {
        const bytes = fromB64(frame.data);
        term.write(bytes);
      } else if (frame.type === "exit") {
        setExitInfo(frame.code);
        term.write(
          `\r\n[process exited with code ${frame.code ?? "?"}]\r\n`,
        );
        if (onExit) onExit(frame.code);
      }
    };

    const onData = term.onData((data) => {
      if (ws.readyState !== WebSocket.OPEN) return;
      const bytes = new TextEncoder().encode(data);
      ws.send(JSON.stringify({ type: "stdin", data: toB64(bytes) }));
    });

    const onResizeTerm = term.onResize(({ rows, cols }) => {
      if (ws.readyState !== WebSocket.OPEN) return;
      ws.send(JSON.stringify({ type: "resize", rows, cols }));
    });

    const onWindowResize = () => {
      try {
        fit.fit();
      } catch {
        // ignore — happens when container is hidden
      }
    };
    window.addEventListener("resize", onWindowResize);

    return () => {
      window.removeEventListener("resize", onWindowResize);
      onData.dispose();
      onResizeTerm.dispose();
      try {
        ws.close();
      } catch {
        // ignore
      }
      term.dispose();
      termRef.current = null;
      fitRef.current = null;
      wsRef.current = null;
    };
  }, [sessionId, onExit]);

  return (
    <div className="flex flex-col h-full">
      <div className="flex items-center justify-between border-b border-border bg-card px-3 py-1 text-xs text-muted-foreground">
        <span className="font-mono">{sessionId}</span>
        <span>
          {status === "open" && exitInfo === "open" && (
            <span className="text-emerald-500">● live</span>
          )}
          {status === "open" && exitInfo !== "open" && (
            <span className="text-amber-500">
              ● exited (code {exitInfo ?? "?"})
            </span>
          )}
          {status === "connecting" && (
            <span className="text-muted-foreground">connecting…</span>
          )}
          {status === "closed" && exitInfo === "open" && (
            <span className="text-rose-500">● disconnected</span>
          )}
        </span>
      </div>
      <div
        ref={containerRef}
        className="flex-1 min-h-[400px] bg-black p-1"
        aria-label="Terminal output"
      />
    </div>
  );
}
