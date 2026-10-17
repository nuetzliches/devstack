// Apply the saved theme before the first paint (no flash). A file of its own,
// not an inline script: the hub's Content-Security-Policy allows scripts from
// its own origin only (`script-src 'self'`).
try {
  var t = localStorage.getItem('devstack.theme');
  if (t === 'light' || t === 'dark') document.documentElement.dataset.theme = t;
} catch (e) {}
