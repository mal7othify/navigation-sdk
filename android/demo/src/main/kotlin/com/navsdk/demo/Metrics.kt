package com.navsdk.demo

/** Boundary-crossing cost summary over the most recent samples. */
data class BoundaryStats(
    val calls: Int = 0,
    val avgUs: Double = 0.0,
    val p99Us: Double = 0.0,
    val maxUs: Double = 0.0,
) {
    companion object {
        /** Summarise nanosecond samples; `window` keeps the last N for the percentile. */
        fun of(samplesNanos: List<Long>, totalCalls: Int): BoundaryStats {
            if (samplesNanos.isEmpty()) return BoundaryStats(calls = totalCalls)
            val sorted = samplesNanos.sorted()
            val p99Index = ((sorted.size - 1) * 0.99).toInt()
            return BoundaryStats(
                calls = totalCalls,
                avgUs = sorted.average() / 1000.0,
                p99Us = sorted[p99Index] / 1000.0,
                maxUs = sorted.last() / 1000.0,
            )
        }
    }
}

/** Everything the metrics strip shows for one approach. */
data class ApproachMetrics(
    val boundary: BoundaryStats = BoundaryStats(),
    /** Position emissions seen by the UI layer. */
    val positionUpdates: Int = 0,
    /** Trip emissions seen by the UI layer (the instruction card's inputs). */
    val tripUpdates: Int = 0,
    /** Actionable reroute requests issued by the core. */
    val rerouteRequests: Int = 0,
    /** Routes installed (host-driven for pull, core-driven for push). */
    val routesInstalled: Int = 0,
    /** Push only: replans currently awaiting the host. */
    val replanInFlight: Boolean = false,
)

/** Great-circle distance in metres, for the "diverged" check in Both mode. */
fun haversineM(lat1: Double, lng1: Double, lat2: Double, lng2: Double): Double {
    val r = 6_371_008.8
    val dLat = Math.toRadians(lat2 - lat1)
    val dLng = Math.toRadians(lng2 - lng1)
    val a = Math.sin(dLat / 2) * Math.sin(dLat / 2) +
        Math.cos(Math.toRadians(lat1)) * Math.cos(Math.toRadians(lat2)) *
        Math.sin(dLng / 2) * Math.sin(dLng / 2)
    return 2 * r * Math.asin(Math.sqrt(a))
}
