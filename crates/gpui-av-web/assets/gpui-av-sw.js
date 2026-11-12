// The gpui-av service worker: it sends the request headers that a <video>
// element cannot.
//
// A page plays a source with headers from a made-up URL under this worker's
// scope: <scope>__gpui-av__/<id>/<name>. The worker answers such a request
// by fetching the real URL with the source's headers and the request's
// Range header. It learns the real URL and the headers by asking the page
// that made the request, and keeps them in memory only.
//
// Serve this file from the directory of the page (its scope must cover the
// page), then start the backend with WebBackend::with_service_worker.

const MARKER = '/__gpui-av__/';
const ASK_TIMEOUT_MS = 3000;

// id -> { url, headers: [[name, value], ...] }
const sources = new Map();

self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', (event) => event.waitUntil(self.clients.claim()));

self.addEventListener('message', (event) => {
  const message = event.data;
  if (message && message.gpuiAv === 'forget') {
    sources.delete(message.id);
  }
});

self.addEventListener('fetch', (event) => {
  const url = new URL(event.request.url);
  const at = url.pathname.indexOf(MARKER);
  if (url.origin !== self.location.origin || at < 0) {
    return;
  }
  const id = url.pathname.slice(at + MARKER.length).split('/')[0];
  event.respondWith(answer(event, id));
});

function ask(client, id) {
  return new Promise((resolve) => {
    const channel = new MessageChannel();
    const timer = setTimeout(() => resolve(null), ASK_TIMEOUT_MS);
    channel.port1.onmessage = (reply) => {
      clearTimeout(timer);
      resolve(reply.data);
    };
    client.postMessage({ gpuiAv: 'lookup', id }, [channel.port2]);
  });
}

async function lookup(id, clientId) {
  const known = sources.get(id);
  if (known) {
    return known;
  }
  // The page that made the request knows; any other page of this origin
  // may, when the request names no client.
  const asked = clientId ? await self.clients.get(clientId) : null;
  const clients = asked ? [asked] : await self.clients.matchAll({ type: 'window' });
  for (const client of clients) {
    const source = await ask(client, id);
    if (source && typeof source.url === 'string') {
      sources.set(id, source);
      return source;
    }
  }
  return null;
}

async function answer(event, id) {
  const source = await lookup(id, event.clientId);
  if (!source) {
    return new Response('gpui-av: no page knows this source', { status: 404 });
  }
  const headers = new Headers(source.headers || []);
  const range = event.request.headers.get('range');
  if (range) {
    headers.set('range', range);
  }
  try {
    return await fetch(source.url, {
      headers,
      mode: 'cors',
      credentials: 'same-origin',
      signal: event.request.signal,
    });
  } catch (error) {
    return Response.error();
  }
}
