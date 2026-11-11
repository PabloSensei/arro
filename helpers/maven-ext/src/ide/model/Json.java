package ide.model;

import java.util.Iterator;
import java.util.List;
import java.util.Map;

/** Helpers run inside the build's JVM, so they carry no libraries that could clash with it. */
final class Json {
    private Json() {
    }

    static String write(Object value) {
        StringBuilder out = new StringBuilder();
        write(value, out);
        return out.toString();
    }

    private static void write(Object value, StringBuilder out) {
        if (value instanceof Map) {
            writeObject((Map<?, ?>) value, out);
        } else if (value instanceof List) {
            writeArray((List<?>) value, out);
        } else if (value instanceof String) {
            writeString((String) value, out);
        } else {
            out.append(value);
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

    private static void writeArray(List<?> list, StringBuilder out) {
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
            } else if (c < 0x20) {
                out.append(String.format("\\u%04x", (int) c));
            } else {
                out.append(c);
            }
        }
        out.append('"');
    }
}
