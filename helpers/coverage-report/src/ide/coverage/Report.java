package ide.coverage;

import java.io.File;
import java.io.FileDescriptor;
import java.io.FileOutputStream;
import java.io.PrintStream;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.Iterator;
import java.util.List;
import java.util.Map;
import java.util.TreeMap;

import org.jacoco.core.analysis.Analyzer;
import org.jacoco.core.analysis.CoverageBuilder;
import org.jacoco.core.analysis.IClassCoverage;
import org.jacoco.core.analysis.ICounter;
import org.jacoco.core.analysis.ICoverageNode;
import org.jacoco.core.analysis.ILine;
import org.jacoco.core.analysis.IMethodCoverage;
import org.jacoco.core.tools.ExecFileLoader;

/**
 * Reads the data file of a JaCoCo agent and prints, as one JSON object, the lines of every
 * source and the counters of every type. Runs outside the IDE, so it carries no libraries
 * of its own besides the ones next to it.
 *
 * <p>Usage: {@code java -cp <jars>/* ide.coverage.Report <file.exec> <classes>...}
 */
public final class Report {
    private Report() {
    }

    public static void main(String[] args) throws Exception {
        if (args.length < 2) {
            System.err.println("usage: ide.coverage.Report <file.exec> <classes>...");
            System.exit(2);
            return;
        }
        File exec = new File(args[0]);
        if (!exec.isFile()) {
            System.err.println("no coverage data at " + exec);
            System.exit(2);
            return;
        }
        ExecFileLoader loader = new ExecFileLoader();
        loader.load(exec);

        CoverageBuilder builder = new CoverageBuilder();
        Analyzer analyzer = new Analyzer(loader.getExecutionDataStore(), builder);
        for (int index = 1; index < args.length; index++) {
            File classes = new File(args[index]);
            if (!classes.isDirectory()) {
                continue;
            }
            analyzer.analyzeAll(classes);
        }

        Map<String, Source> sources = new TreeMap<String, Source>();
        List<IClassCoverage> classes = new ArrayList<IClassCoverage>(builder.getClasses());
        java.util.Collections.sort(classes, new java.util.Comparator<IClassCoverage>() {
            public int compare(IClassCoverage left, IClassCoverage right) {
                return left.getName().compareTo(right.getName());
            }
        });
        List<String> types = new ArrayList<String>();
        for (IClassCoverage type : classes) {
            String source = sourceOf(type);
            if (source != null) {
                Source file = sources.get(source);
                if (file == null) {
                    file = new Source(source);
                    sources.put(source, file);
                }
                file.add(type);
            }
            types.add(type(type));
        }

        StringBuilder out = new StringBuilder();
        out.append("{\"files\":[");
        Iterator<Source> files = sources.values().iterator();
        while (files.hasNext()) {
            files.next().write(out);
            if (files.hasNext()) {
                out.append(',');
            }
        }
        out.append("],\"types\":[");
        for (int index = 0; index < types.size(); index++) {
            if (index > 0) {
                out.append(',');
            }
            out.append(types.get(index));
        }
        out.append("]}");

        PrintStream stream =
                new PrintStream(new FileOutputStream(FileDescriptor.out), false, StandardCharsets.UTF_8.name());
        stream.print(out);
        stream.flush();
    }

    /** The source of a class relative to the root, `demo/Counter.java`; null for none. */
    private static String sourceOf(IClassCoverage type) {
        String name = type.getSourceFileName();
        if (name == null || name.isEmpty()) {
            return null;
        }
        String packageName = type.getPackageName();
        if (packageName == null || packageName.isEmpty()) {
            return name;
        }
        return packageName.replace('.', '/') + "/" + name;
    }

    /** One type of the tree, with the counters the window shows. */
    private static String type(IClassCoverage type) {
        StringBuilder out = new StringBuilder();
        out.append("{\"name\":");
        Json.string(type.getName().replace('/', '.'), out);
        out.append(",\"package\":");
        Json.string(type.getPackageName(), out);
        out.append(",\"source\":");
        Json.string(sourceOf(type) == null ? "" : sourceOf(type), out);
        out.append(",\"class\":");
        Json.counters(type.getCounter(ICoverageNode.CounterEntity.CLASS), out);
        // Constructors and static initializers are not methods of the source: the window
        // counts the ones the user wrote, as IDEA does.
        int methodsCovered = 0;
        int methodsTotal = 0;
        for (IMethodCoverage method : type.getMethods()) {
            if ("<init>".equals(method.getName()) || "<clinit>".equals(method.getName())) {
                continue;
            }
            methodsCovered += method.getMethodCounter().getCoveredCount();
            methodsTotal += method.getMethodCounter().getTotalCount();
        }
        out.append(",\"method\":[").append(methodsCovered).append(',').append(methodsTotal).append(']');
        out.append(",\"line\":");
        Json.counters(type.getCounter(ICoverageNode.CounterEntity.LINE), out);
        out.append(",\"branch\":");
        Json.counters(type.getCounter(ICoverageNode.CounterEntity.BRANCH), out);
        out.append('}');
        return out.toString();
    }

    /** The lines of one source, merged over the classes it declares. */
    private static final class Source {
        private final String name;
        private final Map<Integer, Line> lines = new TreeMap<Integer, Line>();

        Source(String name) {
            this.name = name;
        }

        void add(IClassCoverage type) {
            for (int number = type.getFirstLine(); number <= type.getLastLine(); number++) {
                ILine line = type.getLine(number);
                if (line == null || line.getStatus() == ICounter.EMPTY) {
                    continue;
                }
                Line merged = lines.get(Integer.valueOf(number));
                if (merged == null) {
                    merged = new Line();
                    lines.put(Integer.valueOf(number), merged);
                }
                merged.hits += line.getInstructionCounter().getCoveredCount();
                merged.branchesCovered += line.getBranchCounter().getCoveredCount();
                merged.branchesTotal += line.getBranchCounter().getTotalCount();
            }
        }

        void write(StringBuilder out) {
            out.append("{\"source\":");
            Json.string(name, out);
            out.append(",\"lines\":[");
            Iterator<Map.Entry<Integer, Line>> entries = lines.entrySet().iterator();
            while (entries.hasNext()) {
                Map.Entry<Integer, Line> entry = entries.next();
                Line line = entry.getValue();
                out.append("{\"line\":").append(entry.getKey().intValue());
                out.append(",\"hits\":").append(line.hits);
                if (line.branchesTotal > 0) {
                    out.append(",\"branches\":[").append(line.branchesCovered);
                    out.append(',').append(line.branchesTotal).append(']');
                }
                out.append('}');
                if (entries.hasNext()) {
                    out.append(',');
                }
            }
            out.append("]}");
        }
    }

    /** The parts of a line that several classes may report. */
    private static final class Line {
        int hits;
        int branchesCovered;
        int branchesTotal;
    }

    /** The few writes of JSON this program needs. */
    private static final class Json {
        private Json() {
        }

        static void counters(ICounter counter, StringBuilder out) {
            out.append('[').append(counter.getCoveredCount()).append(',');
            out.append(counter.getTotalCount()).append(']');
        }

        static void string(String value, StringBuilder out) {
            out.append('"');
            for (int index = 0; index < value.length(); index++) {
                char character = value.charAt(index);
                switch (character) {
                    case '"':
                        out.append("\\\"");
                        break;
                    case '\\':
                        out.append("\\\\");
                        break;
                    case '\n':
                        out.append("\\n");
                        break;
                    case '\r':
                        out.append("\\r");
                        break;
                    case '\t':
                        out.append("\\t");
                        break;
                    default:
                        if (character < 0x20) {
                            out.append(String.format("\\u%04x", Integer.valueOf(character)));
                        } else {
                            out.append(character);
                        }
                        break;
                }
            }
            out.append('"');
        }
    }
}
