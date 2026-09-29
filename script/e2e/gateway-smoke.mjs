import assert from 'node:assert/strict';

const baseUrl = process.env.E2E_BASE_URL ?? 'http://127.0.0.1:18080';
const adminPassword = process.env.E2E_ADMIN_PASSWORD ?? 'admin123';

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

await request('/livez');
const readiness = await request('/readyz');
assert.equal(readiness.status, 'ok');
assert.equal(readiness.checks.database.status, 'ok');
assert.equal(readiness.checks.redis.status, 'ok');
assert.equal(readiness.checks.objectStorage.status, 'ok');

await request('/toonflow/projects', {}, 401);
await request('/infra/config/page', {}, 401);
await request('/infra/capabilities');

const login = await request('/system/auth/login', {
  method: 'POST',
  body: JSON.stringify({
    username: 'admin',
    password: adminPassword,
    tenantId: 1,
  }),
});
const token = login?.data?.access_token;
assert.ok(token, 'login response did not contain an access token');
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

const wsUrl = new URL(baseUrl.replace(/^http/, 'ws'));
wsUrl.pathname = '/socket/scriptAgent';
wsUrl.searchParams.set('token', token);
wsUrl.searchParams.set('isolationKey', `scriptAgent:${projectId}:project`);
wsUrl.searchParams.set('projectId', String(projectId));

await new Promise((resolve, reject) => {
  const socket = new WebSocket(wsUrl);
  let opened = false;
  const timeout = setTimeout(() => {
    socket.close();
    reject(new Error('WebSocket handshake timed out'));
  }, 5000);
  socket.addEventListener('open', () => {
    opened = true;
    clearTimeout(timeout);
    socket.close(1000, 'smoke test complete');
    resolve();
  });
  socket.addEventListener('error', () => {
    clearTimeout(timeout);
    reject(new Error('WebSocket handshake failed'));
  });
  socket.addEventListener('close', () => {
    if (!opened) {
      clearTimeout(timeout);
      reject(new Error('WebSocket closed before opening'));
    }
  });
});

await request('/toonflow/project/delProject', {
  method: 'POST',
  headers: authenticated,
  body: JSON.stringify({ id: projectId }),
});

console.log('gateway HTTP/WebSocket smoke test passed');
