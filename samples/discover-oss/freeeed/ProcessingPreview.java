import com.sun.net.httpserver.HttpExchange;
import com.sun.net.httpserver.HttpServer;
import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.net.InetSocketAddress;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.charset.StandardCharsets;
import java.nio.ByteBuffer;
import java.nio.charset.CharacterCodingException;
import java.nio.file.Files;
import java.nio.file.LinkOption;
import java.nio.file.Path;
import java.time.Duration;
import java.util.Comparator;
import java.util.Set;
import java.util.concurrent.Executors;
import java.util.concurrent.Semaphore;
import java.util.concurrent.TimeUnit;
import java.util.zip.ZipEntry;
import java.util.zip.ZipOutputStream;

/** A deliberately narrow synthetic-file preview, not FreeEed Review. */
public final class ProcessingPreview {
    private static final int MAX_INPUT = 10 * 1024 * 1024;
    private static final long MAX_OUTPUT = 48L * 1024 * 1024;
    private static final Set<String> EXTENSIONS = Set.of("txt");
    private static final Semaphore JOB = new Semaphore(1);
    private static final String PAGE = """
        <!doctype html><html lang="en"><meta charset="utf-8">
        <meta name="viewport" content="width=device-width,initial-scale=1">
        <title>FreeEed processing preview</title>
        <main><h1>FreeEed processing preview</h1>
        <p>Experimental: synthetic files only. This preview does not include review,
        search, OCR, or a verified production workflow.</p>
        __EXTRACTOR_NOTICE__
        <p><a href="/source">Adapter and processing source</a> (dependency notices are incomplete).</p>
        <form><label>Text file (TXT; maximum 10 MiB)
        <input type="file" required accept=".txt"></label>
        <button>Process</button></form><p role="status" id="status"></p></main>
        <script>
        document.querySelector('form').addEventListener('submit',async e=>{
          e.preventDefault();const file=document.querySelector('input').files[0];
          const status=document.querySelector('#status');
          if(!file || file.size>10485760){status.textContent='Choose a file up to 10 MiB.';return;}
          const ext=file.name.split('.').pop().toLowerCase();
          if(ext!=='txt'){status.textContent='Unsupported file type.';return;}
          status.textContent='Processing…';
          try{const response=await fetch('/process',{method:'POST',headers:{
            'Content-Type':'application/octet-stream','X-File-Extension':ext},body:file});
            if(!response.ok){status.textContent='Processing failed or the service is busy.';return;}
            const url=URL.createObjectURL(await response.blob()),a=document.createElement('a');
            a.href=url;a.download='freeeed-results.zip';a.click();
            setTimeout(()=>URL.revokeObjectURL(url),60000);status.textContent='Results downloaded.';
          }catch{status.textContent='Could not reach the processing service.';}
        });</script></html>
        """;

    private final Path jar;
    private final Path work;
    private final Path sourceArchive;
    private final String tika;

    private ProcessingPreview(Path jar, Path work, String tika, Path sourceArchive) throws IOException {
        this.jar = jar.toRealPath();
        this.work = work.toRealPath(LinkOption.NOFOLLOW_LINKS);
        this.tika = tika;
        this.sourceArchive = sourceArchive == null ? null : sourceArchive.toRealPath();
        if (!Files.isRegularFile(this.jar) || !Files.isDirectory(this.work, LinkOption.NOFOLLOW_LINKS)) {
            throw new IOException("invalid runtime paths");
        }
        if (this.sourceArchive != null && (!Files.isRegularFile(this.sourceArchive)
                || Files.size(this.sourceArchive) > 16L * 1024 * 1024)) throw new IOException("invalid source archive");
    }

    public static void main(String[] args) throws Exception {
        if (args.length != 5 && args.length != 6) throw new IllegalArgumentException("jar work host port tika-url [source-zip]");
        ProcessingPreview preview = new ProcessingPreview(Path.of(args[0]), Path.of(args[1]), args[4],
                args.length == 6 ? Path.of(args[5]) : null);
        HttpServer server = HttpServer.create(new InetSocketAddress(args[2], Integer.parseInt(args[3])), 8);
        server.createContext("/", preview::handle);
        server.setExecutor(Executors.newFixedThreadPool(4));
        server.start();
    }

    private void handle(HttpExchange exchange) throws IOException {
        try {
            String path = exchange.getRequestURI().getPath();
            String method = exchange.getRequestMethod();
            if (path.equals("/") && method.equals("GET")) {
                String notice = Boolean.getBoolean("freeeed.preview.directText")
                        ? "<p>Only UTF-8 text is supported; a local text adapter is used instead of Apache Tika.</p>"
                        : "";
                send(exchange, 200, "text/html; charset=utf-8", PAGE.replace("__EXTRACTOR_NOTICE__", notice).getBytes(StandardCharsets.UTF_8));
            } else if (path.equals("/health") && method.equals("GET")) {
                boolean healthy = health();
                send(exchange, healthy ? 200 : 503, "text/plain", (healthy ? "ok\n" : "unavailable\n").getBytes(StandardCharsets.UTF_8));
            } else if (path.equals("/source") && method.equals("GET") && sourceArchive != null) {
                exchange.getResponseHeaders().set("Content-Disposition", "attachment; filename=freeeed-preview-source.zip");
                send(exchange, 200, "application/zip", Files.readAllBytes(sourceArchive));
            } else if (path.equals("/process") && method.equals("POST")) {
                process(exchange);
            } else {
                send(exchange, 404, "text/plain", new byte[0]);
            }
        } catch (Exception failure) {
            send(exchange, 500, "text/plain", "Processing failed.\n".getBytes(StandardCharsets.UTF_8));
        } finally {
            exchange.close();
        }
    }

    private boolean health() {
        try {
            HttpClient client = HttpClient.newBuilder().connectTimeout(Duration.ofSeconds(3)).build();
            HttpRequest request = HttpRequest.newBuilder(URI.create(tika + "/tika"))
                    .timeout(Duration.ofSeconds(5)).GET().build();
            return client.send(request, HttpResponse.BodyHandlers.discarding()).statusCode() == 200;
        } catch (Exception failure) {
            return false;
        }
    }

    private void process(HttpExchange exchange) throws Exception {
        String type = exchange.getRequestHeaders().getFirst("Content-Type");
        String extension = exchange.getRequestHeaders().getFirst("X-File-Extension");
        String length = exchange.getRequestHeaders().getFirst("Content-Length");
        long size;
        try { size = Long.parseLong(length); } catch (RuntimeException invalid) { size = -1; }
        if (!"application/octet-stream".equals(type) || extension == null || !EXTENSIONS.contains(extension)
                || size <= 0 || size > MAX_INPUT) {
            send(exchange, 400, "text/plain", "Invalid input.\n".getBytes(StandardCharsets.UTF_8));
            return;
        }
        if (!JOB.tryAcquire()) {
            send(exchange, 503, "text/plain", "Busy.\n".getBytes(StandardCharsets.UTF_8));
            return;
        }
        Path job = null;
        byte[] resultBytes;
        try {
            byte[] bytes = exchange.getRequestBody().readNBytes((int) size + 1);
            if (bytes.length != size) throw new IOException("incomplete input");
            try {
                String text = StandardCharsets.UTF_8.newDecoder().decode(ByteBuffer.wrap(bytes)).toString();
                if (text.codePoints().anyMatch(c -> Character.isISOControl(c) && c != '\n' && c != '\r' && c != '\t')) {
                    send(exchange, 400, "text/plain", "Invalid text.\n".getBytes(StandardCharsets.UTF_8));
                    return;
                }
            } catch (CharacterCodingException invalid) {
                send(exchange, 400, "text/plain", "Invalid text.\n".getBytes(StandardCharsets.UTF_8));
                return;
            }
            job = Files.createTempDirectory(work, "freeeed-job-");
            Path input = Files.createDirectory(job.resolve("input"));
            Files.write(input.resolve("document." + extension), bytes);
            Path project = job.resolve("fixture.project");
            String properties = "project-name=File processing preview\nproject-code=job\ninput=" + input
                    + "\ndir-active=y\ncustodian=preview\ndata_source=0\nstage=true\nstage-in-place=false"
                    + "\nprocess-where=local\nfile-system=local\nfiles-per-zip-staging=50"
                    + "\ngigs-per-zip-staging=0.1\nfield-separator=pipe\nmetadata-file-ext=CSV"
                    + "\nmetadata=standard\nload_file_format=CSV\ncreate-pdf=false\npreview=false"
                    + "\nlucene_fs_index_enabled=false\nsend_index_solr_enabled=false\nocr_enabled=false"
                    + "\npii_active=false\nsummarize_active=false\ntranscribe=false\n";
            Files.writeString(project, properties);
            Path log = job.resolve("engine.log");
            Process child = new ProcessBuilder("java", "-Djava.awt.headless=true", "-Djava.net.preferIPv4Stack=true",
                    "-Djava.io.tmpdir=" + job, "-Duser.home=" + job, "-Dfreeeed.tika.base=" + tika,
                    "-jar", jar.toString(), "-param_file", project.toString())
                    .directory(job.toFile()).redirectErrorStream(true).redirectOutput(log.toFile()).start();
            if (!child.waitFor(90, TimeUnit.SECONDS)) {
                child.descendants().forEach(ProcessHandle::destroyForcibly);
                child.destroyForcibly();
                child.waitFor();
                throw new IOException("processing timeout");
            }
            if (child.exitValue() != 0 || Files.size(log) > 4 * 1024 * 1024) throw new IOException("engine failed");
            String report = Files.readString(log);
            if (!report.contains("Processing finished") || report.contains("Failed to extract metadata")
                    || report.contains("Exception")) throw new IOException("engine did not complete");
            Path result = job.resolve("FreeEed-output/freeeed-output/job/output/results");
            Path metadata = result.resolve("metadata1.csv"), files = result.resolve("native1.zip");
            if (!Files.isRegularFile(metadata, LinkOption.NOFOLLOW_LINKS)
                    || !Files.isRegularFile(files, LinkOption.NOFOLLOW_LINKS)
                    || Files.size(metadata) + Files.size(files) > MAX_OUTPUT) throw new IOException("invalid results");
            ByteArrayOutputStream buffer = new ByteArrayOutputStream();
            try (ZipOutputStream zip = new ZipOutputStream(buffer)) {
                for (Path item : new Path[]{metadata, files}) {
                    zip.putNextEntry(new ZipEntry(item.getFileName().toString()));
                    Files.copy(item, zip);
                    zip.closeEntry();
                }
            }
            resultBytes = buffer.toByteArray();
        } finally {
            try {
                if (job != null) {
                    try (var paths = Files.walk(job)) {
                        for (Path path : paths.sorted(Comparator.reverseOrder()).toList()) Files.deleteIfExists(path);
                    }
                }
            } finally {
                JOB.release();
            }
        }
        exchange.getResponseHeaders().set("Content-Disposition", "attachment; filename=freeeed-results.zip");
        send(exchange, 200, "application/zip", resultBytes);
    }

    private static void send(HttpExchange exchange, int status, String type, byte[] bytes) throws IOException {
        exchange.getResponseHeaders().set("Content-Type", type);
        exchange.getResponseHeaders().set("Cache-Control", "no-store");
        exchange.getResponseHeaders().set("X-Content-Type-Options", "nosniff");
        exchange.sendResponseHeaders(status, bytes.length);
        try (var output = exchange.getResponseBody()) { output.write(bytes); }
    }
}
