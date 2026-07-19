import { apiFetch, logout as sharedLogout } from "./shared.js";

function app() {
  return {
    token: localStorage.getItem("talos_token"),
    view: "login",
    loginForm: { username: "", password: "" },
    registerForm: { username: "", password: "" },
    loginError: "",
    registerError: "",
    registerMessage: "",
    health: "unknown",
    zones: [],
    zonesError: "",
    newZone: { id: "", kind: "Delay" },
    state: null,
    stateError: "",
    ws: null,

    init() {
      if (this.token) {
        this.checkHealth();
        this.refreshZones();
        this.connectWs();
      }
    },

    connectWs() {
      const protocol = location.protocol === "https:" ? "wss:" : "ws:";
      const socket = new WebSocket(`${protocol}//${location.host}/ws`);
      this.ws = socket;
      socket.addEventListener("open", () => {
        socket.send(this.token);
      });
      socket.addEventListener("message", (event) => {
        const data = JSON.parse(event.data);
        this.state = data.state;
      });
      socket.addEventListener("close", () => {
        if (this.token) {
          setTimeout(() => this.connectWs(), 2000);
        }
      });
    },

    async apiFetch(path, options = {}) {
      return apiFetch(path, options);
    },

    async login() {
      this.loginError = "";
      try {
        const res = await fetch("/auth/login", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(this.loginForm),
        });
        if (!res.ok) {
          this.loginError = `Login failed (HTTP ${res.status})`;
          return;
        }
        const data = await res.json();
        localStorage.setItem("talos_token", data.token);
        this.token = data.token;
        this.loginForm = { username: "", password: "" };
        this.checkHealth();
        this.refreshZones();
        this.connectWs();
      } catch (err) {
        this.loginError = `Login failed: ${err}`;
      }
    },

    async register() {
      this.registerError = "";
      this.registerMessage = "";
      try {
        const res = await this.apiFetch("/auth/register", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(this.registerForm),
        });
        if (!res.ok) {
          this.registerError = `Registration failed (HTTP ${res.status})`;
          return;
        }
        this.registerMessage = "Registered. You can now log in.";
        this.registerForm = { username: "", password: "" };
        this.view = "login";
      } catch (err) {
        this.registerError = `Registration failed: ${err}`;
      }
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
        const res = await this.apiFetch("/zones");
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
        const res = await this.apiFetch("/zones", {
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
        const res = await this.apiFetch(`/zones/${id}`, { method: "DELETE" });
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
        const res = await this.apiFetch("/arm", { method: "POST" });
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
        const res = await this.apiFetch("/disarm", { method: "POST" });
        if (!res.ok) {
          this.stateError = `Failed to disarm (HTTP ${res.status})`;
        }
      } catch (err) {
        // apiFetch already logged out on 401; nothing further to do here.
      }
    },

    logout() {
      this.token = null;
      this.view = "login";
      this.zones = [];
      this.state = null;
      if (this.ws) {
        this.ws.close();
        this.ws = null;
      }
      sharedLogout();
    },
  };
}

window.app = app;
