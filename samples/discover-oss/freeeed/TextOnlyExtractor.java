import com.sun.net.httpserver.HttpExchange;
import com.sun.net.httpserver.HttpServer;
import java.io.IOException;
import java.net.InetSocketAddress;
import java.nio.ByteBuffer;
import java.nio.charset.CharacterCodingException;
import java.nio.charset.StandardCharsets;

/** A loopback-only text adapter, not a replacement for Apache Tika. */
public final class TextOnlyExtractor {
    private static final int MAX_BYTES = 10 * 1024 * 1024;

    public static void main(String[] args) throws Exception {
        if (args.length != 1) throw new IllegalArgumentException("expected port");
        int port = Integer.parseInt(args[0]);
        HttpServer server = HttpServer.create(new InetSocketAddress("127.0.0.1", port), 8);
        server.createContext("/", TextOnlyExtractor::serve);
        server.start();
    }

    private static void serve(HttpExchange exchange) throws IOException {
        try {
            String method = exchange.getRequestMethod();
            String path = exchange.getRequestURI().getPath();
            if (method.equals("GET") && path.equals("/tika")) {
                send(exchange, 200, "text/plain; charset=utf-8", "text-only adapter\n".getBytes(StandardCharsets.UTF_8));
                return;
            }
            if (!method.equals("PUT") || !(path.equals("/tika") || path.equals("/meta"))) {
                send(exchange, 404, "text/plain", new byte[0]);
                return;
            }
            byte[] data = exchange.getRequestBody().readNBytes(MAX_BYTES + 1);
            if (data.length > MAX_BYTES) {
                send(exchange, 413, "text/plain", new byte[0]);
                return;
            }
            String text;
            try {
                text = StandardCharsets.UTF_8.newDecoder().decode(ByteBuffer.wrap(data)).toString();
            } catch (CharacterCodingException invalid) {
                send(exchange, 400, "text/plain", new byte[0]);
                return;
            }
            if (text.codePoints().anyMatch(c -> Character.isISOControl(c) && c != '\n' && c != '\r' && c != '\t')) {
                send(exchange, 400, "text/plain", new byte[0]);
                return;
            }
            byte[] result = path.equals("/tika") ? data
                    : "Content-Type,text/plain; charset=UTF-8\n".getBytes(StandardCharsets.UTF_8);
            send(exchange, 200, path.equals("/tika") ? "text/plain; charset=utf-8" : "text/csv; charset=utf-8", result);
        } finally {
            exchange.close();
        }
    }

    private static void send(HttpExchange exchange, int status, String contentType, byte[] data) throws IOException {
        exchange.getResponseHeaders().set("Content-Type", contentType);
        exchange.getResponseHeaders().set("Cache-Control", "no-store");
        exchange.sendResponseHeaders(status, data.length);
        try (var output = exchange.getResponseBody()) { output.write(data); }
    }
}
