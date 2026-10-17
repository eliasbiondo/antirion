// The console's service worker: it shows the gateway's push notifications and, on a click, opens the
// console page a notification names (only one on the console's own origin).
self.addEventListener('push', function (event) {
  var message = {};
  try {
    message = event.data ? event.data.json() : {};
  } catch (e) {
    message = {};
  }
  var title = typeof message.title === 'string' && message.title ? message.title : 'Antirion';
  event.waitUntil(
    self.registration.showNotification(title, {
      body: typeof message.body === 'string' ? message.body : '',
      tag: typeof message.tag === 'string' ? message.tag : undefined,
      icon: self.registration.scope + 'icon.svg',
      data: { url: typeof message.url === 'string' ? message.url : null },
    }),
  );
});

self.addEventListener('notificationclick', function (event) {
  event.notification.close();
  var url = event.notification.data && event.notification.data.url;
  var target = self.registration.scope;
  try {
    if (url && new URL(url, self.registration.scope).origin === self.location.origin) target = url;
  } catch (e) {
    target = self.registration.scope;
  }
  event.waitUntil(self.clients.openWindow(target));
});
