"""The small requests surface used by core, backed by standard wasi:http."""
import json as _json
from urllib.parse import urlsplit
from componentize_py_types import Err
from wit_world.imports import outgoing_handler, types
from wit_world.imports.streams import StreamError_Closed


class RequestException(Exception):
    pass


class Timeout(RequestException):
    pass


class ConnectionError(RequestException):
    pass


class HTTPError(RequestException):
    pass


class exceptions:
    RequestException = RequestException
    Timeout = Timeout
    ConnectionError = ConnectionError
    HTTPError = HTTPError


class _Response:
    def __init__(self, entry):
        self.status_code = entry["status"]
        self.text = entry.get("text", "")
        self.content = self.text.encode()

    def json(self):
        return _json.loads(self.text)


class Session:
    def __init__(self):
        self.headers = {}

    def request(self, method, url, json=None, timeout=None, **kw):
        try:
            return self._request(method, url, json, timeout, kw.get("headers", {}))
        except Err as exc:
            raise ConnectionError(str(exc.value)) from exc

    def _request(self, method, url, data, timeout, extra_headers):
        url = urlsplit(url)
        if url.scheme not in ("http", "https") or not url.netloc:
            raise ConnectionError("HTTP URL requires a scheme and host")
        headers = {**self.headers, **extra_headers}
        if data is not None:
            headers.setdefault("Content-Type", "application/json")
        fields = types.Fields.from_list([(k, v.encode()) for k, v in headers.items()])
        request = types.OutgoingRequest(fields)
        request.set_method(getattr(types, "Method_" + method.title())())
        request.set_scheme({"http": types.Scheme_Http, "https": types.Scheme_Https}[url.scheme]())
        request.set_authority(url.netloc)
        request.set_path_with_query((url.path or "/") + ("?" + url.query if url.query else ""))
        body = request.body()
        options = types.RequestOptions()
        if timeout is not None:
            ns = int(timeout * 1_000_000_000)
            options.set_connect_timeout(ns)
            options.set_first_byte_timeout(ns)
            options.set_between_bytes_timeout(ns)
        # Start consumption before writing, so a large request cannot fill the pipe.
        with outgoing_handler.handle(request, options) as future:
            with body.write() as stream:
                encoded = _json.dumps(data).encode() if data is not None else b""
                for offset in range(0, len(encoded), 4096):
                    stream.blocking_write_and_flush(encoded[offset:offset + 4096])
            types.OutgoingBody.finish(body, None)
            with future.subscribe() as pollable:
                pollable.block()
            result = future.get()
            if isinstance(result, Err):
                raise result
            result = result.value
            if isinstance(result, Err):
                raise result
            with result.value as response:
                status = response.status()
                with response.consume() as incoming:
                    with incoming.stream() as stream:
                        chunks = []
                        while True:
                            try:
                                chunks.append(stream.blocking_read(65536))
                            except Err as exc:
                                if not isinstance(exc.value, StreamError_Closed):
                                    raise
                                break
                return _Response({"status": status, "text": b"".join(chunks).decode()})


def request(method, url, **kw):
    return Session().request(method, url, **kw)
