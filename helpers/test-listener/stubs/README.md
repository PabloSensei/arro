Signatures of the JUnit Platform launcher API that the listener calls, so that the helper
builds with nothing but a JDK. They are compiled next to the listener and left out of the
jar: at run time the real classes of the project's JUnit Platform are used.
