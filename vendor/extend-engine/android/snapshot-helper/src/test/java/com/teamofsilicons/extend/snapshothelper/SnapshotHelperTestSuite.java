package com.teamofsilicons.extend.snapshothelper;

public final class SnapshotHelperTestSuite {
  private SnapshotHelperTestSuite() {}

  public static void main(String[] args) throws Exception {
    PointerEventScheduleTest.run();
    AccessibilityCaptureStabilizerTest.run();
    BoundedUiAutomationConnectionTest.run();
    GestureViewportReaderTest.run();
  }
}
