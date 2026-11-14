package ide.db;

import java.util.ArrayList;
import java.util.Iterator;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/**
 * A small JSON reader and writer. The helper runs inside a project JVM and carries no
 * libraries that could clash with the driver or the project.
 */
final class Json {
    private Json() {
    }

    static String write(Object value) {
        StringBuilder out = new StringBuilder();
        write(value, out);
        return out.toString();
    }

    private static void write(Object value, StringBuilder out) {
        if (value == null) {
            out.append("null");
        } else if (value instanceof Map) {
            writeObject((Map<?, ?>) value, out);
        } else if (value instanceof Iterable) {
            writeArray((Iterable<?>) value, out);
        } else if (value instanceof String) {
            writeString((String) value, out);
        } else if (value instanceof Boolean) {
            out.append(value);
        } else if (value instanceof Double || value instanceof Float) {
            double number = ((Number) value).doubleValue();
            if (Double.isNaN(number) || Double.isInfinite(number)) {
                writeString(String.valueOf(number), out);
            } else {
                out.append(number);
            }
        } else if (value instanceof Number) {
            out.append(value);
        } else {
            writeString(String.valueOf(value), out);
        }
    }

    private static void writeObject(Map<?, ?> map, StringBuilder out) {
        out.append('{');
        Iterator<? extends Map.Entry<?, ?>> entries = map.entrySet().iterator();
        while (entries.hasNext()) {
            Map.Entry<?, ?> entry = entries.next();
            writeString(String.valueOf(entry.getKey()), out);
            out.append(':');
            write(entry.getValue(), out);
            if (entries.hasNext()) {
                out.append(',');
            }
        }
        out.append('}');
    }

    private static void writeArray(Iterable<?> list, StringBuilder out) {
        out.append('[');
        Iterator<?> items = list.iterator();
        while (items.hasNext()) {
            write(items.next(), out);
            if (items.hasNext()) {
                out.append(',');
            }
        }
        out.append(']');
    }

    private static void writeString(String text, StringBuilder out) {
        out.append('"');
        for (int i = 0; i < text.length(); i++) {
            char c = text.charAt(i);
            if (c == '"' || c == '\\') {
                out.append('\\').append(c);
            } else if (c == '\n') {
                out.append("\\n");
            } else if (c == '\r') {
                out.append("\\r");
            } else if (c == '\t') {
                out.append("\\t");
            } else if (c < 0x20) {
                out.append(String.format("\\u%04x", (int) c));
            } else {
                out.append(c);
            }
        }
        out.append('"');
    }

    static Object parse(String text) {
        Parser parser = new Parser(text);
        Object value = parser.value();
        parser.space();
        if (!parser.done()) {
            throw new IllegalArgumentException("trailing text in JSON");
        }
        return value;
    }

    private static final class Parser {
        private final String text;
        private int at;

        Parser(String text) {
            this.text = text;
        }

        boolean done() {
            return at >= text.length();
        }

        void space() {
            while (at < text.length() && Character.isWhitespace(text.charAt(at))) {
                at++;
            }
        }

        Object value() {
            space();
            if (done()) {
                throw new IllegalArgumentException("the end of JSON");
            }
            char c = text.charAt(at);
            switch (c) {
                case '{':
                    return object();
                case '[':
                    return array();
                case '"':
                    return string();
                case 't':
                    word("true");
                    return Boolean.TRUE;
                case 'f':
                    word("false");
                    return Boolean.FALSE;
                case 'n':
                    word("null");
                    return null;
                default:
                    return number();
            }
        }

        private void word(String expected) {
            if (!text.startsWith(expected, at)) {
                throw new IllegalArgumentException("not " + expected);
            }
            at += expected.length();
        }

        private Map<String, Object> object() {
            Map<String, Object> map = new LinkedHashMap<String, Object>();
            at++;
            space();
            if (!done() && text.charAt(at) == '}') {
                at++;
                return map;
            }
            while (true) {
                space();
                String key = string();
                space();
                if (done() || text.charAt(at) != ':') {
                    throw new IllegalArgumentException("no colon");
                }
                at++;
                map.put(key, value());
                space();
                if (done()) {
                    throw new IllegalArgumentException("the end of an object");
                }
                char c = text.charAt(at++);
                if (c == '}') {
                    return map;
                }
                if (c != ',') {
                    throw new IllegalArgumentException("not a comma");
                }
            }
        }

        private List<Object> array() {
            List<Object> list = new ArrayList<Object>();
            at++;
            space();
            if (!done() && text.charAt(at) == ']') {
                at++;
                return list;
            }
            while (true) {
                list.add(value());
                space();
                if (done()) {
                    throw new IllegalArgumentException("the end of an array");
                }
                char c = text.charAt(at++);
                if (c == ']') {
                    return list;
                }
                if (c != ',') {
                    throw new IllegalArgumentException("not a comma");
                }
            }
        }

        private String string() {
            if (done() || text.charAt(at) != '"') {
                throw new IllegalArgumentException("not a string");
            }
            at++;
            StringBuilder out = new StringBuilder();
            while (at < text.length()) {
                char c = text.charAt(at++);
                if (c == '"') {
                    return out.toString();
                }
                if (c != '\\') {
                    out.append(c);
                    continue;
                }
                char escape = text.charAt(at++);
                switch (escape) {
                    case '"':
                    case '\\':
                    case '/':
                        out.append(escape);
                        break;
                    case 'b':
                        out.append('\b');
                        break;
                    case 'f':
                        out.append('\f');
                        break;
                    case 'n':
                        out.append('\n');
                        break;
                    case 'r':
                        out.append('\r');
                        break;
                    case 't':
                        out.append('\t');
                        break;
                    case 'u':
                        out.append((char) Integer.parseInt(text.substring(at, at + 4), 16));
                        at += 4;
                        break;
                    default:
                        throw new IllegalArgumentException("escape " + escape);
                }
            }
            throw new IllegalArgumentException("the end of a string");
        }

        private Object number() {
            int start = at;
            while (at < text.length()) {
                char c = text.charAt(at);
                if ((c >= '0' && c <= '9') || c == '-' || c == '+' || c == '.' || c == 'e'
                        || c == 'E') {
                    at++;
                } else {
                    break;
                }
            }
            String number = text.substring(start, at);
            if (number.indexOf('.') < 0 && number.indexOf('e') < 0 && number.indexOf('E') < 0) {
                try {
                    return Long.valueOf(number);
                } catch (NumberFormatException ignored) {
                    // A number that does not fit becomes a double below.
                }
            }
            return Double.valueOf(number);
        }
    }
}
