// Permanently redirects every request to the same path and query on https://fennec-lang.dev.
export default {
  fetch(request) {
    const url = new URL(request.url);
    url.protocol = 'https:';
    url.hostname = 'fennec-lang.dev';
    url.port = '';
    return Response.redirect(url.toString(), 301);
  },
};
