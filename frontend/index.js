import { apiFetch, getToken } from "./shared.js";

function app() {
  return {
    view: "login",
    loginForm: { username: "", password: "" },
    registerForm: { username: "", password: "" },
    loginError: "",
    registerError: "",
    registerMessage: "",

    init() {
      if (getToken()) {
        window.location.href = "dashboard.html";
      }
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
        window.location.href = "dashboard.html";
      } catch (err) {
        this.loginError = `Login failed: ${err}`;
      }
    },

    async register() {
      this.registerError = "";
      this.registerMessage = "";
      try {
        const res = await apiFetch("/auth/register", {
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
  };
}

window.app = app;
