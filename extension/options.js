// Whence browser sensor — options page logic. Load/save the receiver bearer
// token (chrome.storage.local, key "token"); the background worker picks up
// changes live via chrome.storage.onChanged.

const input = document.getElementById("token");
const status = document.getElementById("status");

chrome.storage.local.get("token").then(({ token }) => {
  input.value = token || "";
});

document.getElementById("save").addEventListener("click", async () => {
  await chrome.storage.local.set({ token: input.value.trim() });
  status.textContent = "Saved";
  setTimeout(() => (status.textContent = ""), 1800);
});
