package org.junit.platform.launcher;

import java.util.Optional;
import org.junit.platform.engine.TestSource;

public final class TestIdentifier {
    public String getUniqueId() { throw new UnsupportedOperationException(); }

    public Optional<String> getParentId() { throw new UnsupportedOperationException(); }

    public String getDisplayName() { throw new UnsupportedOperationException(); }

    public boolean isTest() { throw new UnsupportedOperationException(); }

    public Optional<TestSource> getSource() { throw new UnsupportedOperationException(); }
}
