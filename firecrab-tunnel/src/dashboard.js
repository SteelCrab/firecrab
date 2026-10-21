"use strict";
const issue = document.getElementById("issue");
const result = document.getElementById("result");
const token = document.getElementById("token");
const address = document.getElementById("address");
const status = document.getElementById("status");
const error = document.getElementById("error");
const revoke = document.getElementById("revoke");
let busy = false;
async function mutate(path, body, success) {
  if (busy) return;
  busy = true;
  issue.disabled = revoke.disabled = true;
  error.hidden = true;
  status.textContent = "Working…";
  try {
    const response = await fetch(path, { method: "POST", credentials: "omit", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body), cache: "no-store" });
    if (!response.ok) {
      const messages = { 401: "This token is invalid.", 429: "Token creation is limited to 16 per IP address per hour. Try again later.", 503: "The connection service is unavailable. Try again later." };
      throw new Error(messages[response.status] || "The request could not be completed.");
    }
    await success(response);
  } catch (failure) {
    status.textContent = "";
    error.textContent = failure.message;
    error.hidden = false;
  } finally { busy = false; issue.disabled = revoke.disabled = false; }
}
issue.addEventListener("click", () => {
  if (!result.hidden && !window.confirm("Your current token will leave this page. Copy it before creating another token.")) return;
  void mutate("/dashboard/tokens", {}, async response => {
    const value = await response.json();
    token.value = value.token;
    token.type = "password";
    address.value = value.publicUrl;
    document.getElementById("reveal").textContent = "Show token";
    document.getElementById("reveal").setAttribute("aria-pressed", "false");
    result.hidden = false;
    status.textContent = "Token created. Copy it into the Firecrab app to connect.";
    token.focus();
  });
});
document.getElementById("copy").addEventListener("click", async () => {
  try { await navigator.clipboard.writeText(token.value); status.textContent = "Token copied."; }
  catch { token.focus(); token.select(); status.textContent = "Copy the selected token manually."; }
});
document.getElementById("reveal").addEventListener("click", event => {
  const reveal = token.type === "password";
  token.type = reveal ? "text" : "password";
  event.currentTarget.textContent = reveal ? "Hide token" : "Show token";
  event.currentTarget.setAttribute("aria-pressed", String(reveal));
});
document.getElementById("revoke-form").addEventListener("submit", event => {
  event.preventDefault();
  const input = document.getElementById("revoke-token");
  const value = input.value.trim();
  if (!/^[a-fA-F0-9]{64}$/.test(value)) { error.textContent = "Enter a valid connection token."; error.hidden = false; return; }
  if (!window.confirm("Revoke this token and stop its public connection?")) return;
  void mutate("/dashboard/revoke", { token: value }, async () => {
    input.value = "";
    if (token.value === value) { token.value = ""; address.value = ""; result.hidden = true; }
    status.textContent = "Token revoked. It can no longer refresh or connect this address.";
  });
});
window.addEventListener("pagehide", () => { token.value = ""; address.value = ""; result.hidden = true; document.getElementById("revoke-token").value = ""; });
