// The little that HTMX does not do: toasts, local times, the drawer's keys, tabs and shortcuts.
(() => {
  const TOAST_MS = 8000;
  const TOAST_MAX = 4;

  function toast({ title, message, level = "info", sticky = false, pre = false }) {
    const error = level === "error";
    const stack = document.getElementById("toasts");
    const tile = document.createElement("div");
    tile.className = error ? "toast error" : "toast";
    tile.setAttribute("role", error ? "alert" : "status");
    const head = document.createElement("div");
    head.className = "toast-head";
    const t = document.createElement("div");
    t.className = "toast-title";
    t.textContent = title;
    const close = document.createElement("button");
    close.className = "icon small";
    close.textContent = "✕";
    close.setAttribute("aria-label", "Close");
    close.addEventListener("click", () => tile.remove());
    head.append(t, close);
    const body = document.createElement(pre ? "pre" : "div");
    body.className = pre ? "raw-json" : "toast-body";
    body.textContent = message;
    tile.append(head, body);
    stack.prepend(tile);
    while (stack.children.length > TOAST_MAX) stack.lastElementChild.remove();
    if (!sticky) setTimeout(() => tile.remove(), TOAST_MS);
  }

  // Server times are UTC; show them in the reader's clock.
  function localTimes(root) {
    root.querySelectorAll("time[data-ts]").forEach((node) => {
      const d = new Date(node.dataset.ts);
      if (!Number.isNaN(d.getTime())) node.textContent = d.toLocaleTimeString();
    });
  }

  document.body.addEventListener("toast", (e) => toast(e.detail));
  document.body.addEventListener("htmx:afterSwap", (e) => localTimes(e.target));
  document.body.addEventListener("htmx:responseError", (e) => {
    toast({ title: "Request failed", message: e.detail.xhr.responseText || String(e.detail.xhr.status), level: "error" });
  });
  localTimes(document);

  function selectRow(id) {
    document.querySelectorAll("#call-list .call[data-id]").forEach((li) => li.classList.toggle("sel", li.dataset.id === String(id)));
  }
  function closeDrawer() {
    const drawer = document.getElementById("drawer");
    if (!drawer) return;
    drawer.hidden = true;
    selectRow(null);
  }

  document.addEventListener("click", (e) => {
    const call = e.target.closest("#call-list .call[data-id]");
    if (call) selectRow(call.dataset.id);
    if (e.target.closest("[data-close-drawer]")) closeDrawer();

    const tab = e.target.closest(".tab-btn[data-tab]");
    if (tab) {
      const form = tab.closest("form");
      if (form && form.elements.tab) form.elements.tab.value = tab.dataset.tab;
      document.querySelectorAll(".tab-btn").forEach((b) => b.classList.toggle("on", b === tab));
      document.querySelectorAll("#tab-body, #tab-json").forEach((c) => c.classList.toggle("on", c.id === `tab-${tab.dataset.tab}`));
    }

    const cmp = e.target.closest(".cmp[data-cmp]");
    if (cmp && !e.target.closest(".cmp-detail, .cmp-votes")) cmp.classList.toggle("open");

    const cell = e.target.closest("[data-toast-title]");
    if (cell) toast({ title: cell.dataset.toastTitle, message: cell.dataset.toastBody, sticky: true, pre: true });
  });

  // After a swap the selected row and the open drawer may have changed.
  document.body.addEventListener("htmx:afterSettle", (e) => {
    const drawer = document.getElementById("drawer");
    if (drawer && !drawer.hidden && drawer.dataset.id) selectRow(drawer.dataset.id);
  });

  document.addEventListener("keydown", (e) => {
    const drawer = document.getElementById("drawer");
    if (drawer && !drawer.hidden) {
      if (e.key === "Escape") closeDrawer();
      const typing = e.target.closest?.("input, textarea, select, [contenteditable]");
      const tabs = [...drawer.querySelectorAll(".item-tab")];
      if (!typing && !e.metaKey && !e.ctrlKey && !e.altKey && tabs.length > 1 && (e.key === "ArrowRight" || e.key === "ArrowLeft")) {
        e.preventDefault();
        const current = tabs.findIndex((t) => t.classList.contains("on"));
        tabs[(current + (e.key === "ArrowRight" ? 1 : -1) + tabs.length) % tabs.length].click();
      }
    }
    if (e.metaKey || e.ctrlKey) {
      if (e.key === "Enter") { const b = document.getElementById("send-btn"); if (b) { e.preventDefault(); b.click(); } }
      if (e.key === "s") { const b = document.getElementById("save-btn"); if (b) { e.preventDefault(); b.click(); } }
    }
  });
})();
