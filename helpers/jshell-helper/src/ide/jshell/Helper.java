package ide.jshell;

import java.io.BufferedReader;
import java.io.ByteArrayInputStream;
import java.io.FileDescriptor;
import java.io.FileOutputStream;
import java.io.InputStreamReader;
import java.io.OutputStream;
import java.io.PrintStream;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.concurrent.BlockingQueue;
import java.util.concurrent.LinkedBlockingQueue;

import jdk.jshell.Diag;
import jdk.jshell.ImportSnippet;
import jdk.jshell.JShell;
import jdk.jshell.JShellException;
import jdk.jshell.MethodSnippet;
import jdk.jshell.Snippet;
import jdk.jshell.SnippetEvent;
import jdk.jshell.SourceCodeAnalysis;
import jdk.jshell.VarSnippet;

/**
 * The console helper of the IDE: one console, one JSON line per request and answer.
 *
 * <p>The main thread reads the requests, so a stop reaches a fragment that still runs.
 * A second thread owns the console; it splits a text into fragments, runs them one by
 * one and reports what each of them was, what it returned and what it printed. The
 * fragments run in a process of their own, so their output never mixes with the
 * protocol of this helper.
 */
public final class Helper {
    private static final int MAX_VALUE = 10000;
    private static final String ENGINE = "jdi";

    private static PrintStream out;
    private static final BlockingQueue<Map<String, Object>> requests =
            new LinkedBlockingQueue<Map<String, Object>>();
    private static volatile JShell shell;
    private static volatile boolean stopRequested;
    private static volatile long currentId;
    private static String classpath;

    private Helper() {
    }

    public static void main(String[] args) throws Exception {
        out = new PrintStream(new FileOutputStream(FileDescriptor.out), true, "UTF-8");
        classpath = classpath(args);
        Thread worker = new Thread(new Runnable() {
            @Override
            public void run() {
                work();
            }
        }, "console-helper-worker");
        worker.start();
        BufferedReader in = new BufferedReader(new InputStreamReader(System.in, "UTF-8"));
        String line;
        while ((line = in.readLine()) != null) {
            Map<String, Object> request;
            try {
                Object parsed = Json.parse(line);
                if (!(parsed instanceof Map)) {
                    continue;
                }
                @SuppressWarnings("unchecked")
                Map<String, Object> object = (Map<String, Object>) parsed;
                request = object;
            } catch (RuntimeException malformed) {
                continue;
            }
            String type = string(request, "type");
            if ("stop".equals(type)) {
                stopRequested = true;
                JShell running = shell;
                if (running != null) {
                    try {
                        running.stop();
                    } catch (RuntimeException ignored) {
                        // The fragment may have ended between the read and the call.
                    }
                }
                continue;
            }
            if ("close".equals(type)) {
                requests.offer(request);
                break;
            }
            requests.offer(request);
        }
        requests.offer(map("type", "close"));
        worker.join(10000L);
        System.exit(0);
    }

    private static String classpath(String[] args) {
        for (int index = 0; index + 1 < args.length; index++) {
            if ("--classpath".equals(args[index])) {
                return args[index + 1];
            }
        }
        return null;
    }

    private static void work() {
        try {
            shell = create();
        } catch (Throwable error) {
            emit(map("type", "error", "id", Long.valueOf(0),
                    "message", message(error)));
            return;
        }
        emit(map("type", "ready", "protocol", Long.valueOf(1),
                "runtime", System.getProperty("java.version", ""), "engine", ENGINE));
        while (true) {
            Map<String, Object> request;
            try {
                request = requests.take();
            } catch (InterruptedException ended) {
                return;
            }
            String type = string(request, "type");
            long id = number(request, "id");
            try {
                if ("close".equals(type)) {
                    close();
                    return;
                }
                if ("eval".equals(type)) {
                    eval(id, string(request, "text"));
                } else if ("reset".equals(type)) {
                    reset();
                    emit(map("type", "done", "id", Long.valueOf(id),
                            "cancelled", Boolean.FALSE));
                } else if ("complete".equals(type)) {
                    complete(id, string(request, "text"), (int) number(request, "cursor"));
                }
            } catch (Throwable error) {
                emit(map("type", "error", "id", Long.valueOf(id),
                        "message", message(error)));
            }
        }
    }

    private static JShell create() throws Exception {
        JShell created = JShell.builder()
                // The fragments run in a child JVM, so their output does not share the
                // standard streams of this helper.
                .executionEngine(ENGINE)
                .in(new ByteArrayInputStream(new byte[0]))
                .out(new PrintStream(new StreamSink(false), true, "UTF-8"))
                .err(new PrintStream(new StreamSink(true), true, "UTF-8"))
                .build();
        if (classpath != null && !classpath.isEmpty()) {
            created.addToClasspath(classpath);
        }
        return created;
    }

    private static void reset() throws Exception {
        JShell previous = shell;
        shell = null;
        if (previous != null) {
            try {
                previous.close();
            } catch (RuntimeException ignored) {
                // A console that is already gone needs no second close.
            }
        }
        shell = create();
    }

    private static void close() {
        JShell previous = shell;
        shell = null;
        if (previous != null) {
            try {
                previous.close();
            } catch (RuntimeException ignored) {
                // A console that is already gone needs no second close.
            }
        }
    }

    private static void eval(long id, String text) {
        stopRequested = false;
        currentId = id;
        JShell console = shell;
        if (console == null) {
            emit(map("type", "error", "id", Long.valueOf(id),
                    "message", "the console is not open"));
            emit(map("type", "done", "id", Long.valueOf(id), "cancelled", Boolean.FALSE));
            return;
        }
        List<String> fragments = fragments(console, text);
        int index = 0;
        boolean cancelled = false;
        for (String source : fragments) {
            if (stopRequested) {
                cancelled = true;
                break;
            }
            index++;
            emit(fragment(id, index, source, console));
        }
        currentId = 0;
        emit(map("type", "done", "id", Long.valueOf(id),
                "cancelled", Boolean.valueOf(cancelled || stopRequested)));
    }

    /**
     * Splits a text the way the console expects: every snippet that is complete on its
     * own, and the rest as one fragment that reports its own errors.
     */
    private static List<String> fragments(JShell console, String text) {
        List<String> found = new ArrayList<String>();
        SourceCodeAnalysis analysis = console.sourceCodeAnalysis();
        String remaining = text;
        while (remaining != null && !remaining.trim().isEmpty()) {
            SourceCodeAnalysis.CompletionInfo info = analysis.analyzeCompletion(remaining);
            String source = info.source();
            if (source == null || source.trim().isEmpty()) {
                break;
            }
            found.add(source);
            String next = info.remaining();
            if (next == null || next.equals(remaining)) {
                break;
            }
            remaining = next;
        }
        return found;
    }

    private static Map<String, Object> fragment(
            long id, int index, String source, JShell console) {
        Map<String, Object> answer = map("type", "fragment", "id", Long.valueOf(id),
                "index", Long.valueOf(index), "source", source,
                "status", "UNKNOWN", "kind", "other");
        List<SnippetEvent> events;
        try {
            events = console.eval(source);
        } catch (Throwable error) {
            answer.put("kind", "error");
            answer.put("status", "REJECTED");
            answer.put("exception", exception(error));
            return answer;
        }
        SnippetEvent event = choose(events);
        if (event == null) {
            return answer;
        }
        Snippet snippet = event.snippet();
        answer.put("kind", kind(snippet));
        answer.put("name", snippet.id());
        answer.put("status", event.status().name());
        String value = event.value();
        if (value != null) {
            boolean truncated = value.length() > MAX_VALUE;
            answer.put("value", truncated ? value.substring(0, MAX_VALUE) : value);
            answer.put("truncated", Boolean.valueOf(truncated));
        }
        if (snippet instanceof VarSnippet) {
            answer.put("valueType", ((VarSnippet) snippet).typeName());
            if ("variable".equals(answer.get("kind"))) {
                answer.put("name", declaredName(source));
            }
        }
        if (snippet instanceof MethodSnippet) {
            answer.put("signature", methodSignature((MethodSnippet) snippet, source));
        } else if (snippet instanceof ImportSnippet) {
            answer.put("signature", source.trim());
        } else if (snippet.kind() == Snippet.Kind.TYPE_DECL) {
            answer.put("name", typeName(source));
            answer.put("signature", typeName(source));
        }
        List<Map<String, Object>> diagnostics = new ArrayList<Map<String, Object>>();
        console.diagnostics(snippet).forEach(diag -> diagnostics.add(diagnostic(diag, source)));
        if (!diagnostics.isEmpty()) {
            answer.put("diagnostics", diagnostics);
        }
        JShellException thrown = event.exception();
        if (thrown != null) {
            answer.put("exception", exception(thrown));
        }
        return answer;
    }

    private static SnippetEvent choose(List<SnippetEvent> events) {
        SnippetEvent chosen = null;
        for (SnippetEvent event : events) {
            if (event.snippet() == null) {
                continue;
            }
            chosen = event;
        }
        return chosen;
    }

    private static String kind(Snippet snippet) {
        // The name is read as text: a newer runtime adds sub kinds this helper was not
        // compiled against, and a switch would not see them.
        if ("TEMP_VAR_EXPRESSION_SUBKIND".equals(snippet.subKind().name())) {
            return "expression";
        }
        Snippet.Kind kind = snippet.kind();
        if (kind == Snippet.Kind.IMPORT) {
            return "import";
        }
        if (kind == Snippet.Kind.TYPE_DECL) {
            return "class";
        }
        if (kind == Snippet.Kind.METHOD) {
            return "method";
        }
        if (kind == Snippet.Kind.VAR) {
            return "variable";
        }
        if (kind == Snippet.Kind.EXPRESSION) {
            return "expression";
        }
        if (kind == Snippet.Kind.STATEMENT) {
            return "statement";
        }
        if (kind == Snippet.Kind.ERRONEOUS) {
            return "error";
        }
        return "other";
    }

    private static Map<String, Object> diagnostic(Diag diag, String source) {
        long start = diag.getStartPosition();
        long end = diag.getEndPosition();
        if (start < 0) {
            start = diag.getPosition();
        }
        if (start < 0) {
            start = 0;
        }
        if (end < start) {
            end = start;
        }
        int[] place = place(source, start);
        return map("start", Long.valueOf(start), "end", Long.valueOf(end),
                "line", Long.valueOf(place[0]), "column", Long.valueOf(place[1]),
                "message", diag.getMessage(Locale.ROOT), "code", diag.getCode());
    }

    private static int[] place(String source, long offset) {
        int at = (int) Math.max(0, Math.min(offset, source.length()));
        int line = 1;
        int column = 1;
        for (int index = 0; index < at; index++) {
            if (source.charAt(index) == '\n') {
                line++;
                column = 1;
            } else {
                column++;
            }
        }
        return new int[] {line, column};
    }

    private static Map<String, Object> exception(Throwable error) {
        StringBuilder stack = new StringBuilder();
        StackTraceElement[] frames = error.getStackTrace();
        for (StackTraceElement frame : frames) {
            if (stack.length() > 0) {
                stack.append('\n');
            }
            stack.append("\tat ").append(frame.toString());
        }
        return map("message", message(error), "stack", stack.toString());
    }

    private static void complete(long id, String text, int cursor) {
        JShell console = shell;
        if (console == null) {
            emit(map("type", "error", "id", Long.valueOf(id),
                    "message", "the console is not open"));
            return;
        }
        int at = Math.max(0, Math.min(cursor, text.length()));
        int[] anchor = new int[1];
        List<Map<String, Object>> items = new ArrayList<Map<String, Object>>();
        for (SourceCodeAnalysis.Suggestion suggestion
                : console.sourceCodeAnalysis().completionSuggestions(text, at, anchor)) {
            items.add(map("text", suggestion.continuation(),
                    "matchesType", Boolean.valueOf(suggestion.matchesType())));
        }
        emit(map("type", "completion", "id", Long.valueOf(id), "items", items));
    }

    private static String declaredName(String source) {
        String head = source;
        int equals = head.indexOf('=');
        if (equals >= 0) {
            head = head.substring(0, equals);
        }
        int semicolon = head.indexOf(';');
        if (semicolon >= 0) {
            head = head.substring(0, semicolon);
        }
        String[] parts = head.trim().split("\\s+");
        if (parts.length == 0) {
            return "";
        }
        String name = parts[parts.length - 1];
        while (name.endsWith("[]")) {
            name = name.substring(0, name.length() - 2);
        }
        return name;
    }

    private static String methodSignature(MethodSnippet method, String source) {
        int paren = source.indexOf('(');
        String head = paren >= 0 ? source.substring(0, paren) : source;
        String[] parts = head.trim().split("\\s+");
        String name = parts.length == 0 ? method.id() : parts[parts.length - 1];
        String parameters = method.parameterTypes();
        return name + "(" + (parameters == null ? "" : parameters) + ")";
    }

    private static String typeName(String source) {
        String[] parts = source.trim().split("\\s+");
        for (int index = 0; index + 1 < parts.length; index++) {
            String word = parts[index];
            if ("class".equals(word) || "interface".equals(word) || "enum".equals(word)
                    || "record".equals(word) || "@interface".equals(word)) {
                String name = parts[index + 1];
                int brace = name.indexOf('{');
                if (brace >= 0) {
                    name = name.substring(0, brace);
                }
                return name;
            }
        }
        return source.trim();
    }

    private static String message(Throwable error) {
        String message = error.getMessage();
        if (message == null || message.isEmpty()) {
            return error.toString();
        }
        return message;
    }

    static synchronized void emit(Map<String, Object> answer) {
        out.println(Json.write(answer));
        out.flush();
    }

    static Map<String, Object> map(Object... pairs) {
        Map<String, Object> map = new LinkedHashMap<String, Object>();
        for (int index = 0; index + 1 < pairs.length; index += 2) {
            map.put(String.valueOf(pairs[index]), pairs[index + 1]);
        }
        return map;
    }

    static String string(Map<String, Object> map, String key) {
        Object value = map.get(key);
        return value == null ? "" : String.valueOf(value);
    }

    static long number(Map<String, Object> map, String key) {
        Object value = map.get(key);
        if (value instanceof Number) {
            return ((Number) value).longValue();
        }
        return 0;
    }

    /** Text a fragment printed, sent to the window as it is written. */
    private static final class StreamSink extends OutputStream {
        private final boolean error;

        StreamSink(boolean error) {
            this.error = error;
        }

        @Override
        public void write(int byteValue) {
            write(new byte[] {(byte) byteValue}, 0, 1);
        }

        @Override
        public void write(byte[] bytes, int offset, int length) {
            if (length <= 0) {
                return;
            }
            emit(map("type", "output", "id", Long.valueOf(currentId),
                    "stream", error ? "err" : "out",
                    "text", new String(bytes, offset, length, StandardCharsets.UTF_8)));
        }
    }
}
