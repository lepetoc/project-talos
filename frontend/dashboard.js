import {
  apiFetch,
  getToken,
  requireAuth,
  logout as sharedLogout,
} from "./shared.js";

function dashboard() {
  return {
    health: "unknown",
    zones: [],
    zonesError: "",
    newZone: { id: "", kind: "Delay" },
    state: null,
    stateError: "",
    ws: null,

    init() {
      if (!requireAuth()) {
        return;
      }
      this.checkHealth();
      this.refreshZones();
      this.connectWs();
    },

    connectWs() {
      const protocol = location.protocol === "https:" ? "wss:" : "ws:";
      const socket = new WebSocket(`${protocol}//${location.host}/ws`);
      this.ws = socket;
      socket.addEventListener("open", () => {
        socket.send(getToken());
      });
      socket.addEventListener("message", (event) => {
        const data = JSON.parse(event.data);
        this.state = data.state;
      });
      socket.addEventListener("close", () => {
        if (getToken()) {
          setTimeout(() => this.connectWs(), 2000);
        }
      });
    },

    async checkHealth() {
      this.health = "checking...";
      try {
        const res = await fetch("/health");
        this.health = res.ok ? "reachable" : `unreachable (HTTP ${res.status})`;
      } catch (err) {
        this.health = "unreachable";
      }
    },

    async refreshZones() {
      this.zonesError = "";
      try {
        const res = await apiFetch("/zones");
        if (!res.ok) {
          this.zonesError = `Failed to load zones (HTTP ${res.status})`;
          return;
        }
        this.zones = await res.json();
      } catch (err) {
        // apiFetch already logged out on 401; nothing further to do here.
      }
    },

    async createZone() {
      this.zonesError = "";
      try {
        const res = await apiFetch("/zones", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            id: Number(this.newZone.id),
            kind: this.newZone.kind,
          }),
        });
        if (!res.ok) {
          this.zonesError = `Failed to create zone (HTTP ${res.status})`;
          return;
        }
        this.newZone = { id: "", kind: "Delay" };
        await this.refreshZones();
      } catch (err) {
        // apiFetch already logged out on 401; nothing further to do here.
      }
    },

    async deleteZone(id) {
      this.zonesError = "";
      try {
        const res = await apiFetch(`/zones/${id}`, { method: "DELETE" });
        if (!res.ok) {
          this.zonesError = `Failed to delete zone ${id} (HTTP ${res.status})`;
          return;
        }
        await this.refreshZones();
      } catch (err) {
        // apiFetch already logged out on 401; nothing further to do here.
      }
    },

    async arm() {
      this.stateError = "";
      try {
        const res = await apiFetch("/arm", { method: "POST" });
        if (!res.ok) {
          this.stateError = `Failed to arm (HTTP ${res.status})`;
        }
      } catch (err) {
        // apiFetch already logged out on 401; nothing further to do here.
      }
    },

    async disarm() {
      this.stateError = "";
      try {
        const res = await apiFetch("/disarm", { method: "POST" });
        if (!res.ok) {
          this.stateError = `Failed to disarm (HTTP ${res.status})`;
        }
      } catch (err) {
        // apiFetch already logged out on 401; nothing further to do here.
      }
    },

    logout() {
      if (this.ws) {
        this.ws.close();
        this.ws = null;
      }
      sharedLogout();
    },
  };
}

window.dashboard = dashboard;
