package ide.db;

import java.io.BufferedReader;
import java.io.FileDescriptor;
import java.io.FileOutputStream;
import java.io.InputStreamReader;
import java.io.PrintStream;
import java.sql.Connection;
import java.sql.DriverManager;
import java.sql.ResultSet;
import java.sql.ResultSetMetaData;
import java.sql.PreparedStatement;
import java.sql.SQLException;
import java.sql.Statement;
import java.util.ArrayList;
import java.util.Collections;
import java.util.HashSet;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Properties;
import java.util.Set;
import java.util.concurrent.BlockingQueue;
import java.util.concurrent.LinkedBlockingQueue;

/**
 * The database helper of the IDE: one connection, one JSON line per request and answer.
 *
 * <p>The main thread reads the requests, so a cancel reaches a statement that still runs.
 * A second thread does the work of one connection; while it waits for the next batch of a
 * result it keeps every other request for later, so the window may still ask for a cancel
 * or close the source.
 */
public final class Helper {
    private static final int BATCH_ROWS = 500;
    private static final int BATCH_BYTES = 256 * 1024;
    private static final int CONNECT_TIMEOUT = 10;

    private static PrintStream out;
    private static final BlockingQueue<Map<String, Object>> requests =
            new LinkedBlockingQueue<Map<String, Object>>();
    private static final Set<Long> cancelled = Collections.synchronizedSet(new HashSet<Long>());
    private static volatile Statement running;

    private Helper() {
    }

    public static void main(String[] args) throws Exception {
        out = new PrintStream(new FileOutputStream(FileDescriptor.out), true, "UTF-8");
        BufferedReader in = new BufferedReader(new InputStreamReader(System.in, "UTF-8"));
        Thread worker = new Thread(new Runnable() {
            @Override
            public void run() {
                work();
            }
        }, "database-helper-worker");
        worker.start();
        emit(map("type", "ready", "protocol", Long.valueOf(1), "vendor", "JDBC",
                "version", version()));
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
            long id = number(request, "id");
            if ("cancel".equals(type)) {
                cancelled.add(Long.valueOf(id));
                Statement statement = running;
                if (statement != null) {
                    try {
                        statement.cancel();
                    } catch (SQLException ignored) {
                        // The statement may have finished between the read and the call.
                    }
                }
                requests.offer(request);
            } else if ("close".equals(type)) {
                requests.offer(request);
                break;
            } else {
                requests.offer(request);
            }
        }
        requests.offer(map("type", "close", "id", Long.valueOf(0)));
        worker.join(5000L);
        System.exit(0);
    }

    private static String version() {
        return System.getProperty("java.version", "");
    }

    private static void work() {
        Connection connection = null;
        List<Map<String, Object>> deferred = new ArrayList<Map<String, Object>>();
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
                    closeQuietly(connection);
                    return;
                }
                if ("connect".equals(type)) {
                    connection = connect(request);
                    emit(map("type", "connected", "id", Long.valueOf(id),
                            "product", product(connection),
                            "autoCommit", Boolean.valueOf(connection.getAutoCommit()),
                            "quote", quote(connection)));
                } else if ("query".equals(type)) {
                    if (connection == null) {
                        emitError(id, new SQLException("Not connected"), false);
                    } else {
                        query(connection, request, deferred);
                    }
                } else if ("schema".equals(type)) {
                    if (connection == null) {
                        emitError(id, new SQLException("Not connected"), false);
                    } else {
                        SchemaReader.read(connection, id);
                    }
                } else if ("autocommit".equals(type)) {
                    connection.setAutoCommit(booleanValue(request, "on"));
                    emitDone(id, null, false, false, 0);
                } else if ("commit".equals(type)) {
                    connection.commit();
                    emitDone(id, null, false, false, 0);
                } else if ("rollback".equals(type)) {
                    connection.rollback();
                    emitDone(id, null, false, false, 0);
                } else if ("cancel".equals(type)) {
                    cancelled.remove(Long.valueOf(id));
                }
            } catch (Throwable error) {
                emitError(id, error, cancelled.remove(Long.valueOf(id)));
            } finally {
                if (!deferred.isEmpty()) {
                    requests.addAll(deferred);
                    deferred.clear();
                }
            }
        }
    }

    private static Connection connect(Map<String, Object> request) throws SQLException {
        String url = string(request, "url");
        Properties properties = new Properties();
        Object extra = request.get("properties");
        if (extra instanceof Map) {
            for (Map.Entry<?, ?> entry : ((Map<?, ?>) extra).entrySet()) {
                if (entry.getValue() != null) {
                    properties.setProperty(String.valueOf(entry.getKey()),
                            String.valueOf(entry.getValue()));
                }
            }
        }
        String user = string(request, "user");
        if (!user.isEmpty()) {
            properties.setProperty("user", user);
        }
        Object password = request.get("password");
        if (password != null) {
            properties.setProperty("password", String.valueOf(password));
        }
        try {
            DriverManager.setLoginTimeout(CONNECT_TIMEOUT);
        } catch (RuntimeException ignored) {
            // A driver that refuses the timeout still connects; the window keeps its own.
        }
        return DriverManager.getConnection(url, properties);
    }

    private static String product(Connection connection) {
        try {
            return connection.getMetaData().getDatabaseProductName() + " "
                    + connection.getMetaData().getDatabaseProductVersion();
        } catch (SQLException unknown) {
            return "JDBC";
        }
    }

    /**
     * The string the driver wraps an identifier with. The specification names a space
     * for a driver that quotes no identifier, and the window then writes the name bare.
     */
    private static String quote(Connection connection) {
        try {
            String quote = connection.getMetaData().getIdentifierQuoteString();
            if (quote == null || quote.trim().isEmpty()) {
                return "";
            }
            return quote;
        } catch (SQLException unknown) {
            return "";
        }
    }

    private static void query(
            Connection connection,
            Map<String, Object> request,
            List<Map<String, Object>> deferred) throws SQLException {
        long id = number(request, "id");
        String sql = string(request, "sql");
        long timeoutMs = number(request, "timeoutMs");
        long maxRows = number(request, "maxRows");
        long started = System.nanoTime();
        PreparedStatement statement = connection.prepareStatement(sql);
        running = statement;
        boolean described = false;
        try {
            if (timeoutMs > 0) {
                statement.setQueryTimeout((int) Math.max(1, timeoutMs / 1000));
            }
            // The limit belongs to the statement, not to the text: LIMIT is not a part
            // of every dialect the drivers here speak.
            if (maxRows > 0) {
                statement.setMaxRows((int) Math.min(Integer.MAX_VALUE, maxRows));
            }
            // The columns of a query are known before it runs. Sending them now lets the
            // window cancel a statement that is still computing its first row.
            try {
                ResultSetMetaData known = statement.getMetaData();
                if (known != null && known.getColumnCount() > 0) {
                    emit(map("type", "columns", "id", Long.valueOf(id),
                            "columns", Values.columns(known)));
                    described = true;
                }
            } catch (SQLException unknown) {
                // A driver that describes a statement only after running it sends them then.
            }
            if (!statement.execute()) {
                long updated = statement.getUpdateCount();
                emitDone(id, updated >= 0 ? Long.valueOf(updated) : null, false, false,
                        elapsed(started));
                return;
            }
            ResultSet result = statement.getResultSet();
            try {
                readResult(connection, result, id, deferred, started, described);
            } finally {
                closeQuietly(result);
            }
        } finally {
            if (running == statement) {
                running = null;
            }
            closeQuietly(statement);
        }
    }

    private static void readResult(
            Connection connection,
            ResultSet result,
            long id,
            List<Map<String, Object>> deferred,
            long started,
            boolean described) throws SQLException {
        ResultSetMetaData meta = result.getMetaData();
        int count = meta.getColumnCount();
        int[] types = new int[count];
        for (int index = 0; index < count; index++) {
            types[index] = meta.getColumnType(index + 1);
        }
        if (!described) {
            emit(map("type", "columns", "id", Long.valueOf(id), "columns", Values.columns(meta)));
        }
        while (true) {
            List<Object> rows = new ArrayList<Object>();
            int bytes = 0;
            while (rows.size() < BATCH_ROWS) {
                if (!result.next()) {
                    break;
                }
                List<Object> row = new ArrayList<Object>();
                for (int index = 0; index < count; index++) {
                    row.add(Values.read(result, index + 1, types[index]));
                }
                rows.add(row);
                bytes += Values.size(row);
                if (bytes >= BATCH_BYTES) {
                    break;
                }
            }
            boolean more = rows.size() >= BATCH_ROWS || bytes >= BATCH_BYTES;
            if (!rows.isEmpty()) {
                emit(map("type", "rows", "id", Long.valueOf(id), "rows", rows));
            }
            if (!more) {
                emitDone(id, null, false, false, elapsed(started));
                return;
            }
            while (true) {
                Map<String, Object> next;
                try {
                    next = requests.take();
                } catch (InterruptedException ended) {
                    return;
                }
                String type = string(next, "type");
                long nextId = number(next, "id");
                if ("fetch".equals(type) && nextId == id) {
                    break;
                }
                if ("cancel".equals(type) && nextId == id) {
                    cancelled.remove(Long.valueOf(id));
                    emitDone(id, null, false, true, elapsed(started));
                    return;
                }
                if ("close".equals(type)) {
                    deferred.add(next);
                    return;
                }
                deferred.add(next);
            }
        }
    }

    private static long elapsed(long started) {
        return (System.nanoTime() - started) / 1000000L;
    }

    private static void emitDone(
            long id, Long updateCount, boolean truncated, boolean wasCancelled, long elapsed) {
        emit(map("type", "done", "id", Long.valueOf(id), "updateCount", updateCount,
                "truncated", Boolean.valueOf(truncated),
                "cancelled", Boolean.valueOf(wasCancelled),
                "elapsedMs", Long.valueOf(elapsed)));
    }

    private static void emitError(long id, Throwable error, boolean wasCancelled) {
        String message = error.getMessage();
        if (message == null || message.isEmpty()) {
            message = error.toString();
        }
        String state = null;
        Integer code = null;
        if (error instanceof SQLException) {
            SQLException sql = (SQLException) error;
            state = sql.getSQLState();
            if (sql.getErrorCode() != 0) {
                code = Integer.valueOf(sql.getErrorCode());
            }
        }
        Map<String, Object> answer = map("type", "error", "id", Long.valueOf(id),
                "message", message, "cancelled", Boolean.valueOf(wasCancelled));
        answer.put("sqlState", state);
        answer.put("code", code);
        emit(answer);
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

    static boolean booleanValue(Map<String, Object> map, String key) {
        Object value = map.get(key);
        return value instanceof Boolean && ((Boolean) value).booleanValue();
    }

    private static void closeQuietly(AutoCloseable closeable) {
        if (closeable == null) {
            return;
        }
        try {
            closeable.close();
        } catch (Exception ignored) {
            // A connection that is already gone needs no second close.
        }
    }
}
