import type { WsChannel, WsClientMessage, WsServerMessage } from "@/lib/types";

type MessageHandler = (msg: WsServerMessage) => void;

export class WsClient {
  private ws: WebSocket | null = null;
  private handlers = new Set<MessageHandler>();
  private subscriptions: WsChannel[] = [];
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private reconnectDelayMs = 1000;
  private maxReconnectDelayMs = 30_000;
  private isClosed = false;

  constructor(private readonly url: string = "/ws") {}

  connect(channels: WsChannel[] = ["agents", "issues", "execution"]) {
    this.subscriptions = channels;
    this.isClosed = false;
    this.open();
  }

  disconnect() {
    this.isClosed = true;
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    this.ws?.close();
    this.ws = null;
  }

  on(handler: MessageHandler) {
    this.handlers.add(handler);
    return () => this.handlers.delete(handler);
  }

  private open() {
    const proto = window.location.protocol === "https:" ? "wss" : "ws";
    const host = window.location.host;
    const url = this.url.startsWith("/")
      ? `${proto}://${host}${this.url}`
      : this.url;

    this.ws = new WebSocket(url);

    this.ws.onopen = () => {
      this.reconnectDelayMs = 1000;
      const sub: WsClientMessage = {
        type: "subscribe",
        channels: this.subscriptions,
      };
      this.ws?.send(JSON.stringify(sub));
    };

    this.ws.onmessage = (event: MessageEvent<string>) => {
      try {
        const msg = JSON.parse(event.data) as WsServerMessage;
        for (const h of this.handlers) h(msg);
      } catch {
        // ignore malformed messages
      }
    };

    this.ws.onclose = () => {
      if (!this.isClosed) this.scheduleReconnect();
    };

    this.ws.onerror = () => {
      this.ws?.close();
    };
  }

  private scheduleReconnect() {
    this.reconnectTimer = setTimeout(() => {
      this.reconnectDelayMs = Math.min(
        this.reconnectDelayMs * 2,
        this.maxReconnectDelayMs,
      );
      this.open();
    }, this.reconnectDelayMs);
  }
}

// Singleton used throughout the app
export const wsClient = new WsClient();
