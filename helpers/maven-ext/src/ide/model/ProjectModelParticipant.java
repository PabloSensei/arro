package ide.model;

import java.io.File;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.HashSet;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Set;

import javax.inject.Inject;
import javax.inject.Named;
import javax.inject.Singleton;

import org.apache.maven.AbstractMavenLifecycleParticipant;
import org.apache.maven.execution.MavenSession;
import org.apache.maven.model.Resource;
import org.apache.maven.project.DefaultDependencyResolutionRequest;
import org.apache.maven.project.DependencyResolutionException;
import org.apache.maven.project.DependencyResolutionRequest;
import org.apache.maven.project.DependencyResolutionResult;
import org.apache.maven.project.MavenProject;
import org.apache.maven.project.ProjectDependenciesResolver;
import org.apache.maven.rtinfo.RuntimeInformation;
import org.eclipse.aether.artifact.Artifact;
import org.eclipse.aether.graph.Dependency;
import org.eclipse.aether.graph.DependencyFilter;
import org.eclipse.aether.graph.DependencyNode;

/**
 * Writes the project model as JSON once the reactor is read. Runs before any plugin, so the
 * model appears even if the build itself fails later. Enabled by -Dide.model.out=FILE.
 */
@Named("ide-project-model")
@Singleton
public class ProjectModelParticipant extends AbstractMavenLifecycleParticipant {
    private static final String OUTPUT_PROPERTY = "ide.model.out";

    @Inject
    private ProjectDependenciesResolver resolver;

    @Inject
    private RuntimeInformation runtime;

    @Override
    public void afterProjectsRead(MavenSession session) {
        String output = session.getUserProperties().getProperty(OUTPUT_PROPERTY);
        if (output == null) {
            return;
        }
        long start = System.nanoTime();
        Map<String, MavenProject> reactor = new HashMap<>();
        for (MavenProject project : session.getProjects()) {
            reactor.put(key(project.getGroupId(), project.getArtifactId(), project.getVersion()), project);
        }

        List<Object> projects = new ArrayList<>();
        for (MavenProject project : session.getProjects()) {
            projects.add(describe(session, project, reactor));
        }

        Map<String, Object> model = new LinkedHashMap<>();
        model.put("schema", 1);
        model.put("tool", "maven");
        model.put("toolVersion", runtime.getMavenVersion());
        model.put("javaVersion", System.getProperty("java.version"));
        model.put("resolveMillis", (System.nanoTime() - start) / 1_000_000);
        model.put("projects", projects);
        try {
            File file = new File(output);
            if (file.getParentFile() != null) {
                Files.createDirectories(file.getParentFile().toPath());
            }
            Files.write(file.toPath(), Json.write(model).getBytes(StandardCharsets.UTF_8));
        } catch (IOException e) {
            throw new IllegalStateException("Cannot write the project model to " + output, e);
        }
    }

    private Map<String, Object> describe(MavenSession session, MavenProject project,
            Map<String, MavenProject> reactor) {
        Map<String, Object> result = new LinkedHashMap<>();
        result.put("id", key(project.getGroupId(), project.getArtifactId(), project.getVersion()));
        result.put("packaging", project.getPackaging());
        result.put("baseDir", project.getBasedir().getPath());
        result.put("sourceRoots", project.getCompileSourceRoots());
        result.put("testSourceRoots", project.getTestCompileSourceRoots());
        result.put("resourceRoots", directories(project.getResources()));
        result.put("testResourceRoots", directories(project.getTestResources()));
        result.put("outputDir", project.getBuild().getOutputDirectory());
        result.put("testOutputDir", project.getBuild().getTestOutputDirectory());

        List<String> problems = new ArrayList<>();
        DependencyResolutionRequest request =
                new DefaultDependencyResolutionRequest(project, session.getRepositorySession())
                        .setResolutionFilter(skipReactorModules(reactor));
        DependencyResolutionResult resolved;
        try {
            resolved = resolver.resolve(request);
        } catch (DependencyResolutionException e) {
            resolved = e.getResult();
            problems.add(String.valueOf(e.getMessage()));
        }

        List<Object> compile = new ArrayList<>();
        List<Object> runtimeScope = new ArrayList<>();
        List<Object> test = new ArrayList<>();
        Set<String> seen = new HashSet<>();
        if (resolved != null && resolved.getDependencyGraph() != null) {
            collect(resolved.getDependencyGraph(), reactor, seen, problems, compile, runtimeScope, test);
        }
        result.put("compileClasspath", compile);
        result.put("runtimeClasspath", runtimeScope);
        result.put("testClasspath", test);
        result.put("problems", problems);
        return result;
    }

    private static List<String> directories(List<Resource> resources) {
        List<String> result = new ArrayList<>();
        for (Resource resource : resources) {
            result.add(resource.getDirectory());
        }
        return result;
    }

    /**
     * An unbuilt neighbour has no file to resolve. Skipping only the resolve step keeps the
     * neighbour's own dependencies in the graph, because the filter does not cut subtrees.
     */
    private static DependencyFilter skipReactorModules(Map<String, MavenProject> reactor) {
        return (node, parents) -> node.getArtifact() == null || !reactor.containsKey(key(node.getArtifact()));
    }

    private static void collect(DependencyNode node, Map<String, MavenProject> reactor, Set<String> seen,
            List<String> problems, List<Object> compile, List<Object> runtimeScope, List<Object> test) {
        Dependency dependency = node.getDependency();
        if (dependency != null && seen.add(node.getArtifact().toString())) {
            Map<String, Object> entry = entry(node.getArtifact(), reactor, problems);
            String scope = dependency.getScope();
            if (!scope.equals("runtime") && !scope.equals("test")) {
                compile.add(entry);
            }
            if (scope.equals("compile") || scope.equals("runtime")) {
                runtimeScope.add(entry);
            }
            test.add(entry);
        }
        for (DependencyNode child : node.getChildren()) {
            collect(child, reactor, seen, problems, compile, runtimeScope, test);
        }
    }

    private static Map<String, Object> entry(Artifact artifact, Map<String, MavenProject> reactor,
            List<String> problems) {
        Map<String, Object> entry = new LinkedHashMap<>();
        MavenProject module = reactor.get(key(artifact));
        if (module != null) {
            boolean tests = "tests".equals(artifact.getClassifier());
            String directory = tests
                    ? module.getBuild().getTestOutputDirectory()
                    : module.getBuild().getOutputDirectory();
            entry.put("kind", "module");
            entry.put("module", key(artifact));
            entry.put("path", directory);
            entry.put("built", new File(directory).isDirectory());
        } else {
            File file = artifact.getFile();
            entry.put("kind", "library");
            entry.put("coordinates", artifact.toString());
            entry.put("path", file == null ? null : file.getPath());
            if (file == null) {
                problems.add("not resolved: " + artifact);
            }
        }
        return entry;
    }

    private static String key(Artifact artifact) {
        return key(artifact.getGroupId(), artifact.getArtifactId(), artifact.getBaseVersion());
    }

    private static String key(String groupId, String artifactId, String version) {
        return groupId + ":" + artifactId + ":" + version;
    }
}
