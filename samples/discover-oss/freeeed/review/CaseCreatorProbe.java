import com.sun.net.httpserver.HttpServer;
import java.net.InetSocketAddress;
import java.nio.charset.StandardCharsets;
import java.lang.reflect.Method;
import java.util.ArrayList;
import java.util.concurrent.atomic.AtomicInteger;
import org.freeeed.util.AutomaticUICaseCreator;

public class CaseCreatorProbe {
    public static void main(String[] args) throws Exception {
        String token = System.getenv("FREEEED_INTERNAL_CASE_TOKEN");
        if (token == null) throw new AssertionError("missing fixture token");
        AtomicInteger mode = new AtomicInteger();
        AtomicInteger calls = new AtomicInteger();
        HttpServer server = HttpServer.create(new InetSocketAddress("127.0.0.1", 8090), 0);
        server.createContext("/freeeedui/usercase.html", exchange -> {
            calls.incrementAndGet();
            if (!token.equals(exchange.getRequestHeaders().getFirst("X-FreeEed-Internal-Token")))
                throw new AssertionError("fixture token was not forwarded");
            byte[] body = mode.get() == 3 ? new byte[4097] :
                (mode.get() == 2 ? "Case created successfully with ID: 42" : "not a case")
                    .getBytes(StandardCharsets.UTF_8);
            int status = mode.get() == 0 ? 302 : 200;
            if (status == 302) exchange.getResponseHeaders().set("Location", "http://127.0.0.1:8090/freeeedui/usercase.html");
            exchange.sendResponseHeaders(status, body.length);
            exchange.getResponseBody().write(body);
            exchange.close();
        });
        server.start();
        try {
            Object creator = new AutomaticUICaseCreator();
            Method method = AutomaticUICaseCreator.class.getDeclaredMethod("sendCase", String.class, java.util.List.class);
            method.setAccessible(true);
            String valid = "http://127.0.0.1:8090/freeeedui/usercase.html";
            for (int i = 0; i < 4; i++) {
                mode.set(i);
                boolean result = (Boolean) method.invoke(creator, valid, new ArrayList<>());
                if (result != (i == 2)) throw new AssertionError("incorrect response acceptance: " + i);
            }
            int before = calls.get();
            if (before != 4) throw new AssertionError("unexpected number of local requests");
            for (String invalid : new String[] {
                "http://127.0.0.1:8091/freeeedui/usercase.html",
                "http://localhost.evil:8090/freeeedui/usercase.html",
                "http://127.0.0.1:8090/freeeedui/usercase.html?extra=1",
                "https://127.0.0.1:8090/freeeedui/usercase.html",
                "http://user@127.0.0.1:8090/freeeedui/usercase.html"
            }) {
                if ((Boolean) method.invoke(creator, invalid, new ArrayList<>())) throw new AssertionError("invalid target accepted");
            }
            if (calls.get() != before) throw new AssertionError("invalid target reached listener");
            System.out.println("Redirect, wrong/oversized body, valid body, and target restrictions passed");
        } finally { server.stop(0); }
    }
}
