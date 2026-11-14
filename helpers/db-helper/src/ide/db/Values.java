package ide.db;

import java.math.BigDecimal;
import java.sql.ResultSet;
import java.sql.ResultSetMetaData;
import java.sql.SQLException;
import java.sql.Types;
import java.util.ArrayList;
import java.util.Base64;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/** Turns the metadata and the values of a driver into the cells of the protocol. */
final class Values {
    static final int MAX_BINARY_BYTES = 4 * 1024;
    static final int MAX_TEXT_BYTES = 64 * 1024;

    private Values() {
    }

    static List<Map<String, Object>> columns(ResultSetMetaData meta) throws SQLException {
        List<Map<String, Object>> columns = new ArrayList<Map<String, Object>>();
        for (int index = 1; index <= meta.getColumnCount(); index++) {
            Map<String, Object> column = new LinkedHashMap<String, Object>();
            column.put("name", meta.getColumnLabel(index));
            column.put("type", meta.getColumnTypeName(index));
            column.put("kind", kind(meta.getColumnType(index)));
            column.put("nullable", meta.isNullable(index) != ResultSetMetaData.columnNoNulls);
            column.put("primaryKey", false);
            columns.add(column);
        }
        return columns;
    }

    static String kind(int type) {
        switch (type) {
            case Types.TINYINT:
            case Types.SMALLINT:
            case Types.INTEGER:
            case Types.BIGINT:
                return "integer";
            case Types.DECIMAL:
            case Types.NUMERIC:
                return "decimal";
            case Types.REAL:
            case Types.FLOAT:
            case Types.DOUBLE:
                return "number";
            case Types.BOOLEAN:
            case Types.BIT:
                return "boolean";
            case Types.DATE:
                return "date";
            case Types.TIME:
            case Types.TIME_WITH_TIMEZONE:
                return "time";
            case Types.TIMESTAMP:
            case Types.TIMESTAMP_WITH_TIMEZONE:
                return "timestamp";
            case Types.BINARY:
            case Types.VARBINARY:
            case Types.LONGVARBINARY:
            case Types.BLOB:
                return "binary";
            case Types.CHAR:
            case Types.VARCHAR:
            case Types.LONGVARCHAR:
            case Types.NCHAR:
            case Types.NVARCHAR:
            case Types.LONGNVARCHAR:
            case Types.CLOB:
            case Types.NCLOB:
                return "text";
            default:
                return "other";
        }
    }

    /** One cell, without a time zone conversion and with binary and large text cut short. */
    static Object read(ResultSet rs, int index, int type) throws SQLException {
        switch (type) {
            case Types.TINYINT:
            case Types.SMALLINT:
            case Types.INTEGER:
                long integer = rs.getLong(index);
                return rs.wasNull() ? null : Long.valueOf(integer);
            case Types.BIGINT: {
                BigDecimal number = rs.getBigDecimal(index);
                if (number == null) {
                    return null;
                }
                try {
                    return Long.valueOf(number.longValueExact());
                } catch (ArithmeticException tooLarge) {
                    return decimal(number);
                }
            }
            case Types.DECIMAL:
            case Types.NUMERIC: {
                BigDecimal number = rs.getBigDecimal(index);
                return number == null ? null : decimal(number);
            }
            case Types.REAL:
            case Types.FLOAT:
            case Types.DOUBLE: {
                double number = rs.getDouble(index);
                return rs.wasNull() ? null : Double.valueOf(number);
            }
            case Types.BOOLEAN:
            case Types.BIT: {
                boolean flag = rs.getBoolean(index);
                return rs.wasNull() ? null : Boolean.valueOf(flag);
            }
            case Types.DATE: {
                java.sql.Date date = rs.getDate(index);
                return date == null ? null : tagged("date", date.toString());
            }
            case Types.TIME:
            case Types.TIME_WITH_TIMEZONE: {
                java.sql.Time time = rs.getTime(index);
                return time == null ? null : tagged("time", time.toString());
            }
            case Types.TIMESTAMP:
            case Types.TIMESTAMP_WITH_TIMEZONE: {
                java.sql.Timestamp timestamp = rs.getTimestamp(index);
                return timestamp == null ? null : tagged("timestamp", timestamp.toString());
            }
            case Types.BINARY:
            case Types.VARBINARY:
            case Types.LONGVARBINARY:
            case Types.BLOB: {
                byte[] bytes = rs.getBytes(index);
                return bytes == null ? null : binary(bytes);
            }
            case Types.CHAR:
            case Types.VARCHAR:
            case Types.LONGVARCHAR:
            case Types.NCHAR:
            case Types.NVARCHAR:
            case Types.LONGNVARCHAR:
            case Types.CLOB:
            case Types.NCLOB: {
                String text = rs.getString(index);
                return text == null ? null : text(text);
            }
            default: {
                Object value = rs.getObject(index);
                if (value == null) {
                    return null;
                }
                if (value instanceof byte[]) {
                    return binary((byte[]) value);
                }
                if (value instanceof BigDecimal) {
                    return decimal((BigDecimal) value);
                }
                if (value instanceof Number) {
                    return value;
                }
                if (value instanceof Boolean) {
                    return value;
                }
                return text(String.valueOf(value));
            }
        }
    }

    private static Map<String, Object> decimal(BigDecimal number) {
        Map<String, Object> value = new LinkedHashMap<String, Object>();
        value.put("decimal", number.toPlainString());
        return value;
    }

    private static Map<String, Object> tagged(String tag, String text) {
        Map<String, Object> value = new LinkedHashMap<String, Object>();
        value.put(tag, text);
        return value;
    }

    private static Map<String, Object> binary(byte[] bytes) {
        int length = Math.min(bytes.length, MAX_BINARY_BYTES);
        byte[] head = new byte[length];
        System.arraycopy(bytes, 0, head, 0, length);
        Map<String, Object> value = new LinkedHashMap<String, Object>();
        value.put("binary", Base64.getEncoder().encodeToString(head));
        value.put("bytes", Long.valueOf(bytes.length));
        value.put("truncated", Boolean.valueOf(bytes.length > MAX_BINARY_BYTES));
        return value;
    }

    private static String text(String text) {
        byte[] bytes = utf8(text);
        if (bytes.length <= MAX_TEXT_BYTES) {
            return text;
        }
        int end = MAX_TEXT_BYTES;
        while (end > 0 && (bytes[end] & 0xC0) == 0x80) {
            end--;
        }
        return new String(bytes, 0, end, java.nio.charset.StandardCharsets.UTF_8);
    }

    private static byte[] utf8(String text) {
        return text.getBytes(java.nio.charset.StandardCharsets.UTF_8);
    }

    /** How many bytes the cells of a row take in the protocol, for the batch limit. */
    static int size(List<Object> row) {
        return Json.write(row).getBytes(java.nio.charset.StandardCharsets.UTF_8).length;
    }
}
