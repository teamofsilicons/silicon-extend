package com.teamofsilicons.extend.display

/** A late image/video callback cannot complete a newer display request. */
class DisplayLoadState {
    data class Result(val requestId: String, val ready: Boolean = false, val error: String? = null)
    @Volatile var result: Result? = null
        private set

    @Synchronized fun begin(requestId: String) { result = Result(requestId) }
    @Synchronized fun complete(requestId: String, error: String? = null) {
        if (result?.requestId == requestId) result = Result(requestId, ready = error == null, error = error)
    }
    @Synchronized fun clear(requestId: String) {
        if (result?.requestId == requestId) result = null
    }
}
