import assert from 'node:assert/strict';

const baseUrl = process.env.E2E_BASE_URL ?? 'http://127.0.0.1:18080';
const adminPassword = process.env.E2E_ADMIN_PASSWORD ?? 'admin123';
// Set by the caller between the two passes: a run that another session
// started is still executing when a new client connects.
const reattachProjectId = process.env.E2E_REATTACH_PROJECT_ID
  ? Number(process.env.E2E_REATTACH_PROJECT_ID)
  : undefined;
const reattachRunId = process.env.E2E_REATTACH_RUN_ID
  ? Number(process.env.E2E_REATTACH_RUN_ID)
  : undefined;
// The first pass leaves its project for the second pass, which deletes it.
const keepProject = process.env.E2E_KEEP_PROJECT === '1';

async function request(path, init = {}, expectedStatus = 200) {
  const response = await fetch(`${baseUrl}${path}`, {
    ...init,
    headers: {
      accept: 'application/json',
      ...(init.body ? { 'content-type': 'application/json' } : {}),
      ...init.headers,
    },
  });
  const text = await response.text();
  let body;
  try {
    body = text ? JSON.parse(text) : undefined;
  } catch {
    body = text;
  }
  assert.equal(
    response.status,
    expectedStatus,
    `${init.method ?? 'GET'} ${path} returned ${response.status}: ${text}`,
  );
  return body;
}

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

async function waitFor(predicate, what, timeoutMs = 10_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const value = predicate();
    if (value) return value;
    await sleep(25);
  }
  throw new Error(`timed out waiting for ${what}`);
}

async function login() {
  const response = await request('/system/auth/login', {
    method: 'POST',
    body: JSON.stringify({
      username: 'admin',
      password: adminPassword,
      tenantId: 1,
    }),
  });
  const token = response?.data?.access_token;
  assert.ok(token, 'login response did not contain an access token');
  return token;
}

function agentSocketUrl(token, projectId, prefix = '') {
  const wsUrl = new URL(baseUrl.replace(/^http/, 'ws'));
  wsUrl.pathname = `${prefix}/socket/scriptAgent`;
  wsUrl.searchParams.set('token', token);
  wsUrl.searchParams.set('isolationKey', `scriptAgent:${projectId}:project`);
  wsUrl.searchParams.set('projectId', String(projectId));
  return wsUrl;
}

/** Opens an agent socket and keeps every frame it receives until it closes. */
function connectAgentSocket(token, projectId, prefix = '') {
  const socket = new WebSocket(agentSocketUrl(token, projectId, prefix));
  const frames = [];
  const closed = new Promise((resolve) => {
    socket.addEventListener('close', resolve);
  });
  socket.addEventListener('message', (event) => {
    try {
      frames.push(JSON.parse(event.data));
    } catch {
      // Heartbeats and other non-JSON frames are not part of the contract.
    }
  });
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => {
      socket.close();
      reject(new Error('WebSocket handshake timed out'));
    }, 5000);
    socket.addEventListener('open', () => {
      clearTimeout(timeout);
      resolve({ closed, frames, socket });
    });
    socket.addEventListener('error', () => {
      clearTimeout(timeout);
      reject(new Error('WebSocket handshake failed'));
    });
    socket.addEventListener('close', () => {
      clearTimeout(timeout);
      reject(new Error('WebSocket closed before opening'));
    });
  });
}

async function closeAgentSocket(session) {
  session.socket.close(1000, 'smoke test complete');
  await session.closed;
}

/** Waits until the session has replayed its opening history or greeting. */
async function settleSession(session) {
  await waitFor(
    () => session.frames.some((frame) => frame.event === 'message'),
    'the opening history or greeting',
  );
  // run:active is sent right after the opening frames on the same stream.
  await sleep(200);
}

async function firstPass() {
  await request('/livez');
  const readiness = await request('/readyz');
  assert.equal(readiness.status, 'ok');
  assert.equal(readiness.checks.database.status, 'ok');
  assert.equal(readiness.checks.redis.status, 'ok');
  assert.equal(readiness.checks.objectStorage.status, 'ok');

  await request('/toonflow/projects', {}, 401);
  await request('/infra/config/page', {}, 401);
  await request('/infra/capabilities');

  const token = await login();
  const authenticated = { authorization: `Bearer ${token}` };

  // Vite/Nginx removes the browser's /api prefix. Exercise that path as well
  // as the original compatibility URL so direct-backend tests cannot hide 404s.
  for (const prefix of ['', '/api']) {
    const post = (path, body = {}) => request(`${prefix}/task/${path}`, {
      method: 'POST', headers: authenticated, body: JSON.stringify(body),
    });
    const page = (await post('getTaskApi', { page: 1, limit: 1 })).data;
    assert.ok(Array.isArray(page.data) && page.data.length <= 1);
    assert.equal(page.total, page.stats.total);
    assert.ok(Array.isArray((await post('getTaskCategories')).data));
    assert.ok(Array.isArray((await post('getProject')).data));
    const id = page.data[0]?.id ?? 0;
    if (process.env.E2E_TASK_ID) assert.equal(id, process.env.E2E_TASK_ID);
    assert.equal((await post('taskDetails', { taskId: id })).data?.id ?? 0, id);
  }

  const me = await request('/system/auth/me', { headers: authenticated });
  assert.equal(me?.data?.username, 'admin');

  const infraConfig = await request('/infra/config/page', {
    headers: authenticated,
  });
  assert.ok(Array.isArray(infraConfig?.data?.list));

  const created = await request('/toonflow/projects', {
    method: 'POST',
    headers: authenticated,
    body: JSON.stringify({ name: `gateway-e2e-${Date.now()}` }),
  });
  const projectId = created?.data?.id;
  assert.ok(Number.isSafeInteger(projectId), 'project creation did not return an integer id');

  const archive = await request(`/toonflow/projects/${projectId}/video-archive`, {
    headers: authenticated,
  });
  assert.ok(Array.isArray(archive?.data?.episodes), 'video archive did not return episodes');

  const session = await connectAgentSocket(token, projectId);
  // A session that opens while nothing is running is not attached to a run.
  await settleSession(session);
  assert.ok(
    !session.frames.some((frame) => frame.event === 'run:active'),
    'a fresh session must not be attached to a run',
  );

  // A chat over the websocket is a durable run. No AI model is configured
  // here, so it fails right away; what matters is that it is recorded, emits
  // its events and reaches a terminal state.
  const frameOffset = session.frames.length;
  session.socket.send(JSON.stringify({ type: 'chat', content: 'durable run smoke' }));
  const started = await waitFor(
    () =>
      session.frames
        .slice(frameOffset)
        .find((frame) => frame.event === 'run:started'),
    'the run:started frame',
  );
  const runId = started.data.runId;
  assert.ok(
    Number.isSafeInteger(runId),
    `run:started carried no run id: ${JSON.stringify(started)}`,
  );
  const reply = await waitFor(
    () =>
      session.frames
        .slice(frameOffset)
        .find(
          (frame) => frame.event === 'message' && frame.data.role === 'assistant',
        ),
    'the reply bubble',
  );
  await waitFor(
    () =>
      session.frames
        .slice(frameOffset)
        .some(
          (frame) =>
            frame.event === 'message:update' &&
            frame.data.id === reply.data.id &&
            ['canceled', 'complete', 'error'].includes(frame.data.status),
        ),
    'the run to reach a terminal state',
    20_000,
  );
  await closeAgentSocket(session);

  const run = await request('/agents/runState', {
    method: 'POST',
    headers: authenticated,
    body: JSON.stringify({ id: runId }),
  });
  assert.ok(
    ['failed', 'success'].includes(run.data.state),
    `run ${runId} ended as ${run.data.state}`,
  );
  const { data: events } = await request('/agents/events', {
    method: 'POST',
    headers: authenticated,
    body: JSON.stringify({ afterId: 0, runId }),
  });
  const eventTypes = events.map((event) => event.eventType);
  assert.equal(eventTypes[0], 'started', `unexpected run events: ${eventTypes}`);
  assert.equal(
    eventTypes.at(-1),
    run.data.state === 'success' ? 'completed' : 'failed',
    `unexpected run events: ${eventTypes}`,
  );

  if (keepProject) return projectId;
  await request('/toonflow/project/delProject', {
    method: 'POST',
    headers: authenticated,
    body: JSON.stringify({ id: projectId }),
  });
  return undefined;
}

async function reattachPass() {
  const token = await login();
  const authenticated = { authorization: `Bearer ${token}` };
  const projectId = reattachProjectId;

  // The run was started by a session that has since disconnected. A new
  // session is told about it and can stop it.
  // The gateway under test allows two requests per path, and the upgrade
  // request counts. This pass needs two connections on top of the one the
  // first pass made, so it uses the equivalent /api alias of the socket.
  const attached = await connectAgentSocket(token, projectId, '/api');
  const active = await waitFor(
    () => attached.frames.find((frame) => frame.event === 'run:active'),
    'the run:active frame',
  );
  assert.equal(active.data.runId, reattachRunId);
  assert.equal(active.data.state, 'running');
  const stopped = await request('/agents/stop', {
    method: 'POST',
    headers: authenticated,
    body: JSON.stringify({ id: reattachRunId }),
  });
  assert.equal(stopped.data.state, 'canceled');
  await closeAgentSocket(attached);

  const fresh = await connectAgentSocket(token, projectId, '/api');
  await settleSession(fresh);
  assert.ok(
    !fresh.frames.some((frame) => frame.event === 'run:active'),
    'a stopped run must not attach to a new session',
  );
  await closeAgentSocket(fresh);

  await request('/toonflow/project/delProject', {
    method: 'POST',
    headers: authenticated,
    body: JSON.stringify({ id: projectId }),
  });
}

let keptProjectId;
if (reattachProjectId && reattachRunId) {
  await reattachPass();
} else {
  keptProjectId = await firstPass();
}

console.log('gateway HTTP/WebSocket smoke test passed');
if (keptProjectId) console.log(JSON.stringify({ projectId: keptProjectId }));
