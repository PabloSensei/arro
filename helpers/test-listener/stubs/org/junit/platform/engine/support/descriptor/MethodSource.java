package org.junit.platform.engine.support.descriptor;

import org.junit.platform.engine.TestSource;

public class MethodSource implements TestSource {
    public String getClassName() { throw new UnsupportedOperationException(); }

    public String getMethodName() { throw new UnsupportedOperationException(); }
}
