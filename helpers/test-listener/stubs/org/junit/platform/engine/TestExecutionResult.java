package org.junit.platform.engine;

import java.util.Optional;

public class TestExecutionResult {
    public enum Status {
        SUCCESSFUL,
        ABORTED,
        FAILED
    }

    public Status getStatus() { throw new UnsupportedOperationException(); }

    public Optional<Throwable> getThrowable() { throw new UnsupportedOperationException(); }
}
