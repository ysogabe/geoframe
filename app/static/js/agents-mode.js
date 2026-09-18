/* Agent status mode: bounded data, textContent-only rendering, no animation. */
const AGENT_STATES = Object.freeze(['live', 'stale', 'unknown']);
const AGENT_LABELS = Object.freeze(['みなも (Hermes)', 'miach-runtime']);
const AGENT_IDS = Object.freeze(['hermes', 'miach']);
const AGENT_TASKS = Object.freeze(['idle', 'coding', 'testing', 'review', 'research', 'maintenance', 'unknown']);
const AGENT_METRICS = Object.freeze(['queue', 'active', 'completed', 'errors', 'uptime_s']);
const AGENT_PIN_GRACE_MS = 5 * 60 * 1000;
let agentsTimer = null;
let agentsNudgeTimer = null;
let agentsGraceTimer = null;

function agentsSafe(value, allowed) { return allowed.includes(value) ? value : 'unknown'; }
function agentsMetrics(value) {
  if (!Array.isArray(value) || value.length > 8) return '不明';
  const rendered = value.map(metric => {
    if (!metric || !AGENT_METRICS.includes(metric.name) ||
        typeof metric.value !== 'number' || !Number.isFinite(metric.value) || metric.value < 0) return null;
    return metric.name + '=' + metric.value;
  });
  return rendered.some(item => item === null) ? '不明' : (rendered.join(', ') || 'なし');
}
function agentsLayer() { return typeof document !== 'undefined' ? document.getElementById('agentslayer') : null; }
function agentsVisible() {
  return typeof modeClaimIsTop === 'function' && modeClaimIsTop('agents');
}
function agentsStop() {
  if (agentsTimer !== null && typeof clearInterval === 'function') clearInterval(agentsTimer);
  if (agentsNudgeTimer !== null && typeof clearTimeout === 'function') clearTimeout(agentsNudgeTimer);
  if (agentsGraceTimer !== null && typeof clearTimeout === 'function') clearTimeout(agentsGraceTimer);
  agentsTimer = null;
  agentsNudgeTimer = null;
  agentsGraceTimer = null;
}
function agentsApplyRotation(payload) {
  if (!payload || payload.rotation_allowed !== false || !agentsVisible() ||
      typeof setTimeout !== 'function') {
    if (payload && payload.rotation_allowed !== false && agentsGraceTimer !== null &&
        typeof clearTimeout === 'function') {
      clearTimeout(agentsGraceTimer); agentsGraceTimer = null;
    }
    return;
  }
  if (agentsGraceTimer !== null) return;
  agentsGraceTimer = setTimeout(() => {
    agentsGraceTimer = null;
    if (agentsVisible() && typeof modeClaimActive === 'function' && modeClaimActive('agents') &&
        typeof keysReleaseMode === 'function') {
      keysReleaseMode('agents', 'local');
      if (typeof keysResolveMode === 'function') keysResolveMode();
    }
  }, AGENT_PIN_GRACE_MS);
}
function agentsRender(payload) {
  const layer = agentsLayer();
  if (!layer) return;
  if (!agentsVisible()) { layer.hidden = true; agentsStop(); return; }
  agentsApplyRotation(payload);
  while (layer.firstChild) layer.removeChild(layer.firstChild);
  layer.hidden = false;
  const title = document.createElement('div'); title.className = 'agents-title'; title.textContent = 'AGENT STATUS'; layer.appendChild(title);
  const list = Array.isArray(payload && payload.agents) ? payload.agents : [];
  list.forEach(agent => {
    const card = document.createElement('section'); card.className = 'agent-card';
    const id = document.createElement('div'); id.className = 'agent-id'; id.textContent = 'ID: ' + agentsSafe(agent && agent.id, AGENT_IDS); card.appendChild(id);
    const label = document.createElement('div'); label.className = 'agent-label'; label.textContent = agentsSafe(agent && agent.label, AGENT_LABELS) === 'unknown' ? '不明' : agent.label; card.appendChild(label);
    const state = document.createElement('div'); state.className = 'agent-state'; state.textContent = agentsSafe(agent && agent.state, AGENT_STATES).toUpperCase(); card.appendChild(state);
    const task = document.createElement('div'); task.className = 'agent-task'; task.textContent = 'TASK: ' + agentsSafe(agent && agent.task_kind, AGENT_TASKS); card.appendChild(task);
    const age = document.createElement('div'); age.className = 'agent-age'; age.textContent = 'AGE: ' + (Number.isInteger(agent && agent.age_s) && agent.age_s >= 0 ? agent.age_s : '?') + 's'; card.appendChild(age);
    const metrics = document.createElement('div'); metrics.className = 'agent-metrics'; metrics.textContent = 'METRICS: ' + agentsMetrics(agent && agent.metrics); card.appendChild(metrics);
    layer.appendChild(card);
  });
}
function agentsPoll() {
  if (!agentsVisible() || typeof fetch !== 'function') { agentsStop(); return; }
  fetch('/api/agents', { method: 'GET', cache: 'no-store' }).then(response => response.json()).then(agentsRender).catch(() => agentsRender({ agents: [], rotation_allowed: false }));
}
function agentsStart() {
  if (!agentsVisible()) { agentsStop(); return; }
  agentsPoll();
  if (typeof setInterval === 'function' && agentsTimer === null) agentsTimer = setInterval(agentsPoll, 5000);
  if (typeof setTimeout === 'function' && agentsNudgeTimer === null) {
    const nudge = () => {
      agentsNudgeTimer = null;
      if (!agentsVisible()) { agentsStop(); return; }
      const layer = agentsLayer();
      if (layer) layer.style.transform = layer.style.transform === 'translate(0.5px, 0)' ? 'translate(0, 0)' : 'translate(0.5px, 0)';
      agentsNudgeTimer = setTimeout(nudge, 60000);
    };
    agentsNudgeTimer = setTimeout(nudge, 60000);
  }
}
if (typeof window !== 'undefined') {
  window.addEventListener('geoframe:mode-change', agentsStart);
  agentsStart();
}
