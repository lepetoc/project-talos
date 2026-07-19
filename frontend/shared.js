export function getToken() {
  return localStorage.getItem("talos_token");
}

export function logout() {
  localStorage.removeItem("talos_token");
  window.location.href = "index.html";
}

export function requireAuth() {
  const token = getToken();
  if (!token) {
    window.location.href = "index.html";
    return null;
  }
  return token;
}

export async function apiFetch(path, options = {}) {
  const token = getToken();
  const headers = { ...(options.headers || {}) };
  if (token) {
    headers["Authorization"] = `Bearer ${token}`;
  }
  const res = await fetch(path, { ...options, headers });
  if (res.status === 401) {
    logout();
    throw new Error("unauthorized");
  }
  return res;
}
