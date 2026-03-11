(function () {
  const data = window.MOCK_REGISTRY;
  if (!data) return;
  const PROCESS_KEY = "ato-local-registry-mock-processes";
  const LOG_KEY = "ato-local-registry-mock-logs";

  const platform = (navigator.platform || "").toLowerCase();
  let currentOs = "linux";
  if (platform.includes("mac")) currentOs = "darwin";
  if (platform.includes("win")) currentOs = "windows";

  const currentArch = platform.includes("arm") ? "arm64" : "x64";
  const currentTarget = `${currentOs}/${currentArch}`;

  function showToast(message) {
    const toast = document.getElementById("toast");
    if (!toast) return;
    toast.textContent = message;
    toast.classList.add("show");
    window.setTimeout(() => toast.classList.remove("show"), 1800);
  }

  async function safeCopy(text) {
    try {
      await navigator.clipboard.writeText(text);
      showToast("Copied. ターミナルに戻って貼り付けて実行してください。");
    } catch {
      showToast("コピーに失敗しました。手動で選択してコピーしてください。");
    }
  }

  function getProcessStore() {
    try {
      return JSON.parse(window.localStorage.getItem(PROCESS_KEY) || "{}");
    } catch {
      return {};
    }
  }

  function setProcessStore(store) {
    window.localStorage.setItem(PROCESS_KEY, JSON.stringify(store));
  }

  function getAliveProcesses() {
    const store = getProcessStore();
    return Object.values(store).filter((proc) => proc.active);
  }

  function getLogStore() {
    try {
      return JSON.parse(window.localStorage.getItem(LOG_KEY) || "{}");
    } catch {
      return {};
    }
  }

  function setLogStore(store) {
    window.localStorage.setItem(LOG_KEY, JSON.stringify(store));
  }

  function appendCapsuleLog(capsuleId, line) {
    const store = getLogStore();
    const current = Array.isArray(store[capsuleId]) ? store[capsuleId] : [];
    current.push(`[${new Date().toLocaleTimeString()}] ${line}`);
    store[capsuleId] = current.slice(-200);
    setLogStore(store);
  }

  function getCapsuleLogs(capsuleId) {
    const store = getLogStore();
    return Array.isArray(store[capsuleId]) ? store[capsuleId] : [];
  }

  function updateTopbarProcessBadge() {
    const badge = document.getElementById("activeProcessBadge");
    if (!badge) return;
    const alive = getAliveProcesses();
    const count = alive.length;
    badge.textContent = `Processes: ${count} active`;
    badge.classList.toggle("status-dead", count === 0);
  }

  function escapeHtml(text) {
    return text
      .replaceAll("&", "&amp;")
      .replaceAll("<", "&lt;")
      .replaceAll(">", "&gt;")
      .replaceAll('"', "&quot;")
      .replaceAll("'", "&#39;");
  }

  function markdownToHtml(md) {
    const lines = md.split("\n");
    return lines
      .map((line) => {
        if (line.startsWith("# ")) return `<h3>${escapeHtml(line.slice(2))}</h3>`;
        if (line.startsWith("## ")) return `<h4>${escapeHtml(line.slice(3))}</h4>`;
        if (line.startsWith("- ")) return `<li>${escapeHtml(line.slice(2))}</li>`;
        if (line.trim() === "") return "";
        return `<p>${escapeHtml(line).replace(/`([^`]+)`/g, "<code>$1</code>")}</p>`;
      })
      .join("");
  }

  function renderCatalogPage() {
    const grid = document.getElementById("catalogGrid");
    if (!grid) return;

    const searchInput = document.getElementById("searchInput");
    const osButtons = Array.from(document.querySelectorAll(".os-pill"));
    const emptyState = document.getElementById("emptyState");
    const copyPublish = document.getElementById("copyPublish");
    const publishCmd = document.getElementById("publishCmd");
    const listViewBtn = document.getElementById("listViewBtn");
    const iconViewBtn = document.getElementById("iconViewBtn");
    const folderPicker = document.getElementById("folderPicker");
    const pickFolderBtn = document.getElementById("pickFolderBtn");
    const publishFolderBtn = document.getElementById("publishFolderBtn");
    const publishInfo = document.getElementById("publishInfo");

    let selectedOs = "all";
    let viewMode = "list";
    let pendingFolder = null;

    function compatibleBadge(capsule) {
      const matched = capsule.osArch.includes(currentTarget);
      return matched
        ? `<span class=\"compat\">✔ Compatible: ${currentTarget}</span>`
        : `<span class=\"compat bad\">✖ Needs: ${capsule.osArch[0]}</span>`;
    }

    function applyViewMode() {
      grid.className = viewMode === "icon" ? "catalog-icon" : "catalog-list";
      listViewBtn?.classList.toggle("active", viewMode === "list");
      iconViewBtn?.classList.toggle("active", viewMode === "icon");
    }

    function render() {
      const q = (searchInput?.value || "").trim().toLowerCase();
      const filtered = data.capsules.filter((capsule) => {
        const searchable = `${capsule.scopedId} ${capsule.description} ${capsule.publisher}`.toLowerCase();
        const matchesQuery = !q || searchable.includes(q);
        const matchesOs =
          selectedOs === "all" || capsule.osArch.some((target) => target.startsWith(`${selectedOs}/`));
        return matchesQuery && matchesOs;
      });

      grid.innerHTML = filtered
        .map(
          (capsule) => `
            <article class="capsule-card" data-id="${capsule.id}">
              <div class="capsule-icon">${capsule.icon || "📦"}</div>
              <div class="capsule-card-main">
                <h3>${capsule.scopedId}</h3>
                <p>${escapeHtml(capsule.description)}</p>
                <div class="card-foot">
                  ${compatibleBadge(capsule)}
                  <span class="version">v${capsule.version}</span>
                </div>
                <div class="capsule-meta">publisher: ${capsule.publisher} · ⚠ unverified</div>
              </div>
            </article>
          `,
        )
        .join("");

      Array.from(grid.querySelectorAll(".capsule-card")).forEach((node) => {
        node.addEventListener("click", () => {
          const id = node.getAttribute("data-id");
          window.location.href = `./capsule.html?id=${encodeURIComponent(id)}`;
        });
      });

      if (emptyState) {
        emptyState.style.display = filtered.length === 0 ? "block" : "none";
      }

      applyViewMode();
    }

    searchInput?.addEventListener("input", render);

    listViewBtn?.addEventListener("click", () => {
      viewMode = "list";
      applyViewMode();
    });

    iconViewBtn?.addEventListener("click", () => {
      viewMode = "icon";
      applyViewMode();
    });

    osButtons.forEach((button) => {
      button.addEventListener("click", () => {
        osButtons.forEach((b) => b.classList.remove("active"));
        button.classList.add("active");
        selectedOs = button.getAttribute("data-os") || "all";
        render();
      });
    });

    copyPublish?.addEventListener("click", () => safeCopy(publishCmd?.textContent || ""));

    pickFolderBtn?.addEventListener("click", () => folderPicker?.click());

    folderPicker?.addEventListener("change", () => {
      const files = Array.from(folderPicker.files || []);
      if (files.length === 0) {
        pendingFolder = null;
        publishFolderBtn.disabled = true;
        return;
      }

      const firstPath = files[0].webkitRelativePath || files[0].name;
      const folderName = firstPath.split("/")[0] || "new-capsule";
      pendingFolder = {
        name: folderName,
        filesCount: files.length,
      };

      if (publishInfo) {
        publishInfo.style.display = "block";
        publishInfo.textContent = `Selected folder: ${folderName} (${files.length} files)`;
      }
      publishFolderBtn.disabled = false;
    });

    publishFolderBtn?.addEventListener("click", () => {
      if (!pendingFolder) return;

      const slug = pendingFolder.name.toLowerCase().replace(/\s+/g, "-");
      const newCapsule = {
        id: `local-${Date.now()}`,
        scopedId: `local/${slug}`,
        name: slug,
        publisher: "local",
        icon: "🆕",
        appUrl: `https://example.com/${slug}`,
        description: `Published from folder ${pendingFolder.name} (${pendingFolder.filesCount} files).`,
        type: "webapp",
        version: "0.1.0",
        size: `${Math.max(1, Math.ceil(pendingFolder.filesCount / 3))}.0 MB`,
        osArch: [currentTarget],
        envHints: ["PORT=3000"],
        readme: `# ${slug}\n\nMock published from local folder.`,
        localPath: `~/.ato/cas/sha256/mock/${slug}.capsule`,
      };

      data.capsules.unshift(newCapsule);
      render();
      showToast("Folder published (mock). New capsule added to catalog.");
      publishFolderBtn.disabled = true;
      pendingFolder = null;
      folderPicker.value = "";
      if (publishInfo) {
        publishInfo.textContent = "Publish completed (mock).";
      }
    });

    render();
    updateTopbarProcessBadge();
    window.setInterval(updateTopbarProcessBadge, 2000);
  }

  function renderCapsulePage() {
    const runStopBtn = document.getElementById("runStopBtn");
    if (!runStopBtn) return;

    const params = new URLSearchParams(window.location.search);
    const id = params.get("id");
    const capsule = data.capsules.find((item) => item.id === id) || data.capsules[0];

    const title = document.getElementById("capsuleTitle");
    const summary = document.getElementById("capsuleSummary");
    const readme = document.getElementById("readmeContent");
    const envHints = document.getElementById("envHints");
    const osArch = document.getElementById("osArch");
    const version = document.getElementById("version");
    const size = document.getElementById("size");
    const compatText = document.getElementById("compatText");
    const localPath = document.getElementById("localPath");
    const runMeta = document.getElementById("runMeta");
    const viewLogsBtn = document.getElementById("viewLogsBtn");
    const processState = document.getElementById("processState");
    const psCommand = document.getElementById("psCommand");
    const mismatchBanner = document.getElementById("mismatchBanner");

    if (title) title.textContent = capsule.scopedId;
    if (summary) summary.textContent = capsule.description;
    if (readme) readme.innerHTML = markdownToHtml(capsule.readme);
    if (envHints) envHints.textContent = capsule.envHints.join("\n");
    if (osArch) osArch.textContent = capsule.osArch.join(", ");
    if (version) version.textContent = `v${capsule.version}`;
    if (size) size.textContent = capsule.size;
    if (compatText) {
      const match = capsule.osArch.includes(currentTarget);
      compatText.textContent = match ? `OK (${currentTarget})` : `Mismatch (${currentTarget})`;
      compatText.style.color = match ? "#166534" : "#991b1b";
    }
    if (mismatchBanner) {
      const match = capsule.osArch.includes(currentTarget);
      if (!match) {
        mismatchBanner.style.display = "block";
        mismatchBanner.textContent = `Architecture Mismatch: required ${capsule.osArch.join(", ")}, current ${currentTarget}`;
      } else {
        mismatchBanner.style.display = "none";
      }
    }
    if (localPath) localPath.textContent = `artifact path: ${capsule.localPath}`;
    if (runMeta) runMeta.textContent = `target: ${capsule.osArch.join(", ")} / registry: ${data.baseUrl}`;

    function getCapsuleProcess() {
      const store = getProcessStore();
      return Object.values(store).find((p) => p.capsuleId === capsule.id) || null;
    }

    function refreshProcessState() {
      const found = getCapsuleProcess();
      const active = Boolean(found?.active);
      if (processState) {
        if (active) {
          processState.textContent = `● Active (pid ${found.pid})`;
          processState.classList.remove("dead");
        } else {
          processState.textContent = "● Inactive";
          processState.classList.add("dead");
        }
      }
      if (psCommand) {
        const pidForCommand = found?.pid || "<pid>";
        psCommand.textContent = `ps -p ${pidForCommand} -o pid,etime,command`;
      }
      runStopBtn.textContent = active ? "Stop" : "Run";
      if (viewLogsBtn) {
        viewLogsBtn.textContent = active ? "View Logs" : "Last Log";
      }
    }

    function startProcessAndOpenApp() {
      const pid = `${Math.floor(Math.random() * 90000) + 10000}`;
      const store = getProcessStore();
      store[pid] = {
        pid,
        capsuleId: capsule.id,
        scopedId: capsule.scopedId,
        active: true,
        startedAt: Date.now(),
        lastSeenAt: Date.now(),
      };
      setProcessStore(store);

      appendCapsuleLog(capsule.id, `starting process ${pid}`);
      appendCapsuleLog(capsule.id, `loading capsule ${capsule.scopedId}`);
      appendCapsuleLog(capsule.id, `launching app URL ${capsule.appUrl || "(not configured)"}`);

      if (capsule.appUrl) {
        window.open(capsule.appUrl, "_blank", "noopener,noreferrer");
      } else {
        showToast("App URL is not configured for this capsule.");
      }

      showToast(`Process started. pid=${pid}`);
      refreshProcessState();
      updateTopbarProcessBadge();
    }

    function stopProcess() {
      const store = getProcessStore();
      const entry = Object.values(store).find((p) => p.capsuleId === capsule.id && p.active);
      if (!entry) {
        showToast("No active process for this capsule.");
        return;
      }
      store[entry.pid].active = false;
      store[entry.pid].lastSeenAt = Date.now();
      setProcessStore(store);
      appendCapsuleLog(capsule.id, `stopped process ${entry.pid}`);
      showToast(`Process stopped. pid=${entry.pid}`);
      refreshProcessState();
      updateTopbarProcessBadge();
    }

    function openLogPage() {
      const found = getCapsuleProcess();
      const pid = found?.pid || "none";
      const logUrl = `./run.html?id=${encodeURIComponent(capsule.id)}&scoped=${encodeURIComponent(capsule.scopedId)}&pid=${encodeURIComponent(pid)}`;
      window.open(logUrl, "_blank", "noopener,noreferrer");
    }

    runStopBtn.addEventListener("click", () => {
      const proc = getCapsuleProcess();
      if (proc?.active) {
        stopProcess();
      } else {
        startProcessAndOpenApp();
      }
    });

    viewLogsBtn?.addEventListener("click", openLogPage);

    document.addEventListener("keydown", (event) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "r") {
        event.preventDefault();
        const proc = getCapsuleProcess();
        if (proc?.active) {
          stopProcess();
        } else {
          startProcessAndOpenApp();
        }
      }
    });

    refreshProcessState();
    updateTopbarProcessBadge();
    window.setInterval(() => {
      refreshProcessState();
      updateTopbarProcessBadge();
    }, 2000);
  }

  function renderRunnerPage() {
    const runnerLog = document.getElementById("runnerLog");
    if (!runnerLog) return;

    const runnerMeta = document.getElementById("runnerMeta");
    const runnerState = document.getElementById("runnerState");
    const params = new URLSearchParams(window.location.search);
    const capsuleId = params.get("id") || "unknown";
    const scoped = params.get("scoped") || capsuleId;
    const pid = params.get("pid") || "none";
    const logs = getCapsuleLogs(capsuleId);

    const store = getProcessStore();
    const active = Object.values(store).some((p) => p.capsuleId === capsuleId && p.active);

    if (runnerMeta) {
      runnerMeta.textContent = `capsule: ${scoped} / pid: ${pid}`;
    }
    if (runnerState) {
      runnerState.textContent = active ? "● Active" : "● Inactive (Last Log)";
      runnerState.classList.toggle("dead", !active);
    }
    runnerLog.textContent = logs.length > 0 ? logs.join("\n") : "No logs yet.";
  }

  renderCatalogPage();
  renderCapsulePage();
  renderRunnerPage();
})();
