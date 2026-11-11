package ide.test;

import java.io.IOException;
import java.io.OutputStream;
import java.io.PrintWriter;
import java.io.StringWriter;
import java.net.InetAddress;
import java.net.Socket;
import java.nio.charset.StandardCharsets;
import java.util.Map;
import java.util.concurrent.ConcurrentHashMap;
import org.junit.platform.engine.TestExecutionResult;
import org.junit.platform.engine.TestSource;
import org.junit.platform.engine.support.descriptor.ClassSource;
import org.junit.platform.engine.support.descriptor.MethodSource;
import org.junit.platform.launcher.TestExecutionListener;
import org.junit.platform.launcher.TestIdentifier;
import org.junit.platform.launcher.TestPlan;

/**
 * Sends what the JUnit Platform runs to the IDE, one JSON object per line over a socket on
 * 127.0.0.1. Found through the service loader, so being on the test classpath is enough.
 * Without a port from the IDE it does nothing: the project's own runs stay as they are.
 */
public final class IdeTestListener implements TestExecutionListener {
    private static final int TRACE_LIMIT = 64 * 1024;
    private static final Object LOCK = new Object();
    private static OutputStream connection;
    private static boolean tried;

    private final Map<String, Long> started = new ConcurrentHashMap<>();

    private static String setting(String property, String variable) {
        String value = System.getProperty(property);
        return value != null ? value : System.getenv(variable);
    }

    /** One connection per JVM, opened at the first event. */
    private static OutputStream connection() {
        if (tried) {
            return connection;
        }
        tried = true;
        String port = setting("ide.test.port", "IDE_TEST_PORT");
        String token = setting("ide.test.token", "IDE_TEST_TOKEN");
        if (port == null || token == null) {
            return null;
        }
        try {
            Socket socket = new Socket(InetAddress.getLoopbackAddress(), Integer.parseInt(port));
            socket.setTcpNoDelay(true);
            connection = socket.getOutputStream();
            connection.write((token + "\n").getBytes(StandardCharsets.UTF_8));
            connection.flush();
        } catch (IOException | NumberFormatException e) {
            System.err.println("ide-test-listener: no connection to the IDE: " + e);
            connection = null;
        }
        return connection;
    }

    private static void send(Json json) {
        synchronized (LOCK) {
            OutputStream out = connection();
            if (out == null) {
                return;
            }
            try {
                out.write((json.end() + "\n").getBytes(StandardCharsets.UTF_8));
                out.flush();
            } catch (IOException e) {
                connection = null;
            }
        }
    }

    @Override
    public void testPlanExecutionStarted(TestPlan testPlan) {
        send(new Json().put("event", "plan"));
    }

    @Override
    public void testPlanExecutionFinished(TestPlan testPlan) {
        send(new Json().put("event", "planFinished"));
    }

    @Override
    public void dynamicTestRegistered(TestIdentifier test) {
        send(describe("registered", test));
    }

    @Override
    public void executionStarted(TestIdentifier test) {
        started.put(test.getUniqueId(), System.nanoTime());
        send(describe("started", test));
    }

    @Override
    public void executionSkipped(TestIdentifier test, String reason) {
        send(describe("skipped", test).put("reason", reason));
    }

    @Override
    public void executionFinished(TestIdentifier test, TestExecutionResult result) {
        Long start = started.remove(test.getUniqueId());
        long millis = start == null ? 0 : (System.nanoTime() - start) / 1_000_000;
        Json json = new Json()
                .put("event", "finished")
                .put("id", test.getUniqueId())
                .put("outcome", outcome(result.getStatus()))
                .put("durationMs", millis);
        Throwable error = result.getThrowable().orElse(null);
        if (error != null) {
            json.put("message", String.valueOf(error.getMessage()));
            json.put("errorType", error.getClass().getName());
            json.put("trace", trace(error));
        }
        send(json);
    }

    private static String outcome(TestExecutionResult.Status status) {
        switch (status) {
            case SUCCESSFUL:
                return "passed";
            case ABORTED:
                return "aborted";
            default:
                return "failed";
        }
    }

    private static Json describe(String event, TestIdentifier test) {
        Json json = new Json()
                .put("event", event)
                .put("id", test.getUniqueId())
                .put("parent", test.getParentId().orElse(null))
                .put("name", test.getDisplayName())
                .put("kind", test.isTest() ? "case" : "suite");
        TestSource source = test.getSource().orElse(null);
        if (source instanceof MethodSource) {
            MethodSource method = (MethodSource) source;
            json.put("suite", method.getClassName()).put("case", method.getMethodName());
        } else if (source instanceof ClassSource) {
            json.put("suite", ((ClassSource) source).getClassName());
        }
        return json;
    }

    private static String trace(Throwable error) {
        StringWriter text = new StringWriter();
        error.printStackTrace(new PrintWriter(text));
        String trace = text.toString();
        return trace.length() > TRACE_LIMIT ? trace.substring(0, TRACE_LIMIT) : trace;
    }

    /** A flat JSON object, enough for the events. */
    private static final class Json {
        private final StringBuilder text = new StringBuilder("{");

        Json put(String name, Object value) {
            if (value == null) {
                return this;
            }
            if (text.length() > 1) {
                text.append(',');
            }
            quote(name);
            text.append(':');
            if (value instanceof Number) {
                text.append(value);
            } else {
                quote(value.toString());
            }
            return this;
        }

        private void quote(String value) {
            text.append('"');
            for (int i = 0; i < value.length(); i++) {
                char c = value.charAt(i);
                switch (c) {
                    case '"':
                        text.append("\\\"");
                        break;
                    case '\\':
                        text.append("\\\\");
                        break;
                    case '\n':
                        text.append("\\n");
                        break;
                    case '\r':
                        text.append("\\r");
                        break;
                    case '\t':
                        text.append("\\t");
                        break;
                    default:
                        if (c < 0x20) {
                            text.append(String.format("\\u%04x", (int) c));
                        } else {
                            text.append(c);
                        }
                }
            }
            text.append('"');
        }

        String end() {
            return text.append('}').toString();
        }
    }
}
