package org.junit.platform.launcher;

import org.junit.platform.engine.TestExecutionResult;

public interface TestExecutionListener {
    default void testPlanExecutionStarted(TestPlan testPlan) {}

    default void testPlanExecutionFinished(TestPlan testPlan) {}

    default void dynamicTestRegistered(TestIdentifier testIdentifier) {}

    default void executionSkipped(TestIdentifier testIdentifier, String reason) {}

    default void executionStarted(TestIdentifier testIdentifier) {}

    default void executionFinished(
            TestIdentifier testIdentifier, TestExecutionResult testExecutionResult) {}
}
