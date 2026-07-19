import { apiFetch, logout as sharedLogout, requireAuth } from "./shared.js";

function settings() {
  return {
    siaAvailable: true,
    siaForm: {
      account: "",
      prefix: "",
      receiver_addr: "",
    },
    siaMessage: "",
    siaError: "",

    shellyAvailable: true,
    shellyForm: {
      gateway_addr: "",
    },
    shellyMessage: "",
    shellyError: "",

    sensors: [],
    sensorsError: "",
    sensorsMessage: "",
    newSensor: {
      sensor_id: "",
      zone_id: "",
    },

    init() {
      if (!requireAuth()) {
        return;
      }
      this.loadSiaConfig();
      this.loadShellyConfig();
    },

    async loadSiaConfig() {
      this.siaError = "";
      try {
        const res = await apiFetch("/modules/sia/config");
        if (res.status === 404) {
          this.siaAvailable = false;
          return;
        }
        if (!res.ok) {
          this.siaError = `Failed to load SIA configuration (HTTP ${res.status})`;
          return;
        }
        const data = await res.json();
        this.siaForm = {
          account: data.account || "",
          prefix: data.prefix || "",
          receiver_addr: data.receiver_addr || "",
        };
        this.siaAvailable = true;
      } catch (err) {
        if (err.message !== "unauthorized") {
          this.siaError = `Failed to load SIA configuration: ${err}`;
        }
      }
    },

    async submitSiaConfig() {
      this.siaMessage = "";
      this.siaError = "";
      try {
        const res = await apiFetch("/modules/sia/config", {
          method: "PUT",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(this.siaForm),
        });
        if (!res.ok) {
          this.siaError = `Failed to save SIA configuration (HTTP ${res.status})`;
          return;
        }
        this.siaMessage = "SIA configuration saved successfully.";
      } catch (err) {
        if (err.message !== "unauthorized") {
          this.siaError = `Failed to save SIA configuration: ${err}`;
        }
      }
    },

    async loadShellyConfig() {
      this.shellyError = "";
      try {
        const res = await apiFetch("/modules/shelly/config");
        if (res.status === 404) {
          this.shellyAvailable = false;
          return;
        }
        if (!res.ok) {
          this.shellyError = `Failed to load Shelly configuration (HTTP ${res.status})`;
          return;
        }
        const data = await res.json();
        this.shellyForm = {
          gateway_addr: data.gateway_addr || "",
        };
        this.shellyAvailable = true;
        await this.refreshSensors();
      } catch (err) {
        if (err.message !== "unauthorized") {
          this.shellyError = `Failed to load Shelly configuration: ${err}`;
        }
      }
    },

    async submitShellyConfig() {
      this.shellyMessage = "";
      this.shellyError = "";
      try {
        const res = await apiFetch("/modules/shelly/config", {
          method: "PUT",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(this.shellyForm),
        });
        if (!res.ok) {
          this.shellyError = `Failed to save Shelly gateway configuration (HTTP ${res.status})`;
          return;
        }
        this.shellyMessage = "Shelly gateway configuration saved successfully.";
      } catch (err) {
        if (err.message !== "unauthorized") {
          this.shellyError = `Failed to save Shelly gateway configuration: ${err}`;
        }
      }
    },

    async refreshSensors() {
      this.sensorsError = "";
      try {
        const res = await apiFetch("/modules/shelly/sensors");
        if (!res.ok) {
          this.sensorsError = `Failed to load sensors (HTTP ${res.status})`;
          return;
        }
        this.sensors = await res.json();
      } catch (err) {
        if (err.message !== "unauthorized") {
          this.sensorsError = `Failed to load sensors: ${err}`;
        }
      }
    },

    async addSensor() {
      this.sensorsMessage = "";
      this.sensorsError = "";
      try {
        const res = await apiFetch("/modules/shelly/sensors", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            sensor_id: this.newSensor.sensor_id,
            zone_id: Number(this.newSensor.zone_id),
          }),
        });
        if (!res.ok) {
          this.sensorsError = `Failed to add sensor (HTTP ${res.status})`;
          return;
        }
        this.newSensor = { sensor_id: "", zone_id: "" };
        this.sensorsMessage = "Sensor mapping added successfully.";
        await this.refreshSensors();
      } catch (err) {
        if (err.message !== "unauthorized") {
          this.sensorsError = `Failed to add sensor: ${err}`;
        }
      }
    },

    async deleteSensor(sensorId) {
      this.sensorsMessage = "";
      this.sensorsError = "";
      try {
        const res = await apiFetch(
          `/modules/shelly/sensors/${encodeURIComponent(sensorId)}`,
          {
            method: "DELETE",
          },
        );
        if (!res.ok) {
          this.sensorsError = `Failed to delete sensor (HTTP ${res.status})`;
          return;
        }
        await this.refreshSensors();
      } catch (err) {
        if (err.message !== "unauthorized") {
          this.sensorsError = `Failed to delete sensor: ${err}`;
        }
      }
    },

    logout() {
      sharedLogout();
    },
  };
}

window.settings = settings;
