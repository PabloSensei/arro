package ide.db;

import java.sql.Connection;
import java.sql.DatabaseMetaData;
import java.sql.ResultSet;
import java.sql.SQLException;
import java.sql.Types;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.LinkedHashSet;
import java.util.List;
import java.util.Map;
import java.util.Set;

/** Reads the schemas, tables, columns, indexes and keys a driver can describe. */
final class SchemaReader {
    /** Tables after which the rest of the schema is left out. */
    private static final int MAX_TABLES = 2000;

    private SchemaReader() {
    }

    static void read(Connection connection, long id) throws SQLException {
        DatabaseMetaData meta = connection.getMetaData();
        boolean[] partial = new boolean[] {false};
        boolean truncated = false;
        int tables = 0;
        ResultSet found = null;
        try {
            found = meta.getTables(null, null, "%", new String[] {"TABLE", "VIEW"});
            while (found.next()) {
                if (tables >= MAX_TABLES) {
                    truncated = true;
                    break;
                }
                tables++;
                String catalog = found.getString("TABLE_CAT");
                String schema = found.getString("TABLE_SCHEM");
                String name = found.getString("TABLE_NAME");
                String type = found.getString("TABLE_TYPE");
                Map<String, Object> table = new LinkedHashMap<String, Object>();
                table.put("schema", schema == null ? "" : schema);
                table.put("name", name);
                table.put("kind", type != null && type.toUpperCase().indexOf("VIEW") >= 0
                        ? "view" : "table");
                table.put("columns", columns(meta, catalog, schema, name, partial));
                table.put("indexes", indexes(meta, catalog, schema, name, partial));
                table.put("foreignKeys", keys(meta, catalog, schema, name, partial));
                table.put("partial", Boolean.FALSE);

                Map<String, Object> answer = new LinkedHashMap<String, Object>();
                answer.put("catalog", catalog);
                answer.put("schema", schema == null ? "" : schema);
                List<Object> list = new ArrayList<Object>();
                list.add(table);
                answer.put("tables", list);
                Helper.emit(Helper.map("type", "schema", "id", Long.valueOf(id), "schema", answer));
            }
        } catch (SQLException error) {
            partial[0] = true;
        } finally {
            if (found != null) {
                try {
                    found.close();
                } catch (SQLException ignored) {
                    // The answer is already known to be partial.
                }
            }
        }
        Helper.emit(Helper.map(
                "type", "schema-done",
                "id", Long.valueOf(id),
                "truncated", Boolean.valueOf(truncated),
                "partial", Boolean.valueOf(partial[0])));
    }

    private static List<Object> columns(
            DatabaseMetaData meta, String catalog, String schema, String table, boolean[] partial) {
        List<Object> columns = new ArrayList<Object>();
        ResultSet found = null;
        try {
            found = meta.getColumns(catalog, schema, table, "%");
            while (found.next()) {
                Map<String, Object> column = new LinkedHashMap<String, Object>();
                column.put("name", found.getString("COLUMN_NAME"));
                column.put("type", found.getString("TYPE_NAME"));
                int type = found.getInt("DATA_TYPE");
                column.put("kind", Values.kind(type));
                column.put("nullable",
                        Boolean.valueOf(found.getInt("NULLABLE") != DatabaseMetaData.columnNoNulls));
                column.put("primaryKey", Boolean.FALSE);
                columns.add(column);
            }
        } catch (SQLException error) {
            partial[0] = true;
        } finally {
            close(found);
        }
        markPrimaryKeys(meta, catalog, schema, table, columns, partial);
        return columns;
    }

    private static void markPrimaryKeys(
            DatabaseMetaData meta,
            String catalog,
            String schema,
            String table,
            List<Object> columns,
            boolean[] partial) {
        Set<String> keys = new LinkedHashSet<String>();
        ResultSet found = null;
        try {
            found = meta.getPrimaryKeys(catalog, schema, table);
            while (found.next()) {
                keys.add(found.getString("COLUMN_NAME"));
            }
        } catch (SQLException error) {
            partial[0] = true;
        } finally {
            close(found);
        }
        for (Object value : columns) {
            Map<?, ?> column = (Map<?, ?>) value;
            if (keys.contains(column.get("name"))) {
                ((Map<String, Object>) value).put("primaryKey", Boolean.TRUE);
            }
        }
    }

    private static List<Object> indexes(
            DatabaseMetaData meta, String catalog, String schema, String table, boolean[] partial) {
        Map<String, Map<String, Object>> byName = new LinkedHashMap<String, Map<String, Object>>();
        ResultSet found = null;
        try {
            found = meta.getIndexInfo(catalog, schema, table, false, true);
            while (found.next()) {
                String name = found.getString("INDEX_NAME");
                if (name == null) {
                    continue;
                }
                Map<String, Object> index = byName.get(name);
                if (index == null) {
                    index = new LinkedHashMap<String, Object>();
                    index.put("name", name);
                    index.put("unique", Boolean.valueOf(!found.getBoolean("NON_UNIQUE")));
                    index.put("columns", new ArrayList<Object>());
                    byName.put(name, index);
                }
                String column = found.getString("COLUMN_NAME");
                if (column != null) {
                    ((List<Object>) index.get("columns")).add(column);
                }
            }
        } catch (SQLException error) {
            partial[0] = true;
        } finally {
            close(found);
        }
        return new ArrayList<Object>(byName.values());
    }

    private static List<Object> keys(
            DatabaseMetaData meta, String catalog, String schema, String table, boolean[] partial) {
        Map<String, Map<String, Object>> byName = new LinkedHashMap<String, Map<String, Object>>();
        ResultSet found = null;
        try {
            found = meta.getImportedKeys(catalog, schema, table);
            while (found.next()) {
                String name = found.getString("FK_NAME");
                if (name == null) {
                    name = table + "_" + found.getString("FKCOLUMN_NAME");
                }
                Map<String, Object> key = byName.get(name);
                if (key == null) {
                    key = new LinkedHashMap<String, Object>();
                    key.put("name", name);
                    key.put("columns", new ArrayList<Object>());
                    key.put("referencedTable", found.getString("PKTABLE_NAME"));
                    key.put("referencedColumns", new ArrayList<Object>());
                    byName.put(name, key);
                }
                String column = found.getString("FKCOLUMN_NAME");
                if (column != null) {
                    ((List<Object>) key.get("columns")).add(column);
                }
                String referenced = found.getString("PKCOLUMN_NAME");
                if (referenced != null) {
                    ((List<Object>) key.get("referencedColumns")).add(referenced);
                }
            }
        } catch (SQLException error) {
            partial[0] = true;
        } finally {
            close(found);
        }
        return new ArrayList<Object>(byName.values());
    }

    private static void close(ResultSet found) {
        if (found == null) {
            return;
        }
        try {
            found.close();
        } catch (SQLException ignored) {
            // Nothing to do with a result the driver already closed.
        }
    }

    /** Kept for a driver that reports a type number instead of a name. */
    static String typeName(int type) {
        return type == Types.NULL ? "NULL" : String.valueOf(type);
    }
}
