import XCTest
@testable import IPhoneUseRemote

final class AssistantVoiceCallTests: XCTestCase {
    func testScreenReadUsesOnlyVisibleLabelsAndRefusesSecureFields() throws {
        let data = Data(#"{"elements":[{"kind":"StaticText","label":"设置"},{"kind":"Button","label":"蓝牙"},{"kind":"StaticText","label":"隐藏内容","visible":false},{"kind":"XCUIElementTypeTextField","label":"私密输入","value":"synthetic-secret"}]}"#.utf8)
        XCTAssertEqual(try DaemonClient.screenText(data), "当前屏幕文字：设置，蓝牙")
        XCTAssertThrowsError(try DaemonClient.screenText(Data(#"{"elements":[{"kind":"SecureTextField","label":"密码"}]}"#.utf8)))
    }
    func testStopAcceptsSpeechPunctuationButDoesNotMatchAnInstructionContainingStop() {
        XCTAssertTrue(AssistantVoiceCall.isStop("停止。"))
        XCTAssertTrue(AssistantVoiceCall.isStop("停止当前任务"))
        XCTAssertTrue(AssistantVoiceCall.isStop("取消当前任务。"))
        XCTAssertTrue(AssistantVoiceCall.isStop(" 停止任务！ "))
        XCTAssertFalse(AssistantVoiceCall.isStop("打开设置，不要停止任务"))
        XCTAssertFalse(AssistantVoiceCall.isStop("搜索停止操作的方法"))
        XCTAssertFalse(AssistantVoiceCall.isStop("确认执行"))
    }

    func testAnnouncementDistinguishesChangedConfirmationAndNewTask() {
        let first = AgentTaskSnapshot(id: "one", goal: "测试", state: .needsConfirm,
                                      confirm: AgentConfirm(action: "CLICK", target: "发送"))
        var changed = first
        changed.confirm = AgentConfirm(action: "CLICK", target: "支付")
        XCTAssertNotEqual(AssistantVoiceCall.announcementKey(first), AssistantVoiceCall.announcementKey(changed))
        changed = first
        changed.id = "two"
        XCTAssertNotEqual(AssistantVoiceCall.announcementKey(first), AssistantVoiceCall.announcementKey(changed))
        changed = first
        changed.elapsedMs = 1000
        XCTAssertEqual(AssistantVoiceCall.announcementKey(first), AssistantVoiceCall.announcementKey(changed))
    }

    @MainActor
    func testHangupIsIdempotentAndDoesNotStartMicrophone() {
        let call = AssistantVoiceCall()
        call.end()
        call.end()
        XCTAssertFalse(call.active)
        XCTAssertEqual(call.status, "通话已结束")
        XCTAssertTrue(call.transcript.isEmpty)
    }
}
