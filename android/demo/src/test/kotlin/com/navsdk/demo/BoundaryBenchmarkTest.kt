package com.navsdk.demo

import com.navsdk.core.NavigatorConfig as PullConfig
import com.navsdk.core.Navigator as PullNavigator
import com.navsdk.core.RouteSimulator as PullSimulator
import com.navsdk.core.SimulatorConfig as PullSimConfig
import com.navsdk.core.routeFromJson as pullRouteFromJson
import com.navsdk.push.core.NavigationController
import com.navsdk.push.core.NavigationListener
import com.navsdk.push.core.NavigatorConfig as PushConfig
import com.navsdk.push.core.PositionState
import com.navsdk.push.core.RouteSimulator as PushSimulator
import com.navsdk.push.core.SessionState
import com.navsdk.push.core.SimulatorConfig as PushSimConfig
import com.navsdk.push.core.TripState
import com.navsdk.push.core.ErrorKind
import com.navsdk.push.core.routeFromJson as pushRouteFromJson
import org.json.JSONObject
import org.junit.Assume.assumeTrue
import org.junit.Test
import java.io.File

/**
 * Boundary-cost microbenchmark, Phase 14: the same simulated drive through
 * the pull (UniFFI + JNA) and push (BoltFFI + JNI) bindings on the JVM.
 *
 * Not a unit test: it prints a table and writes `build/boundary-bench.json`.
 * Run with `scripts/bench-boundary.sh` or
 * `./gradlew :demo:testDebugUnitTest --tests '*BoundaryBenchmarkTest*'`.
 * Skipped when either host library is missing.
 */
class BoundaryBenchmarkTest {
    private val fixtures = File(System.getProperty("navsdk.fixtures") ?: "")
    private val calls = (System.getProperty("navsdk.bench.calls") ?: "10000").toInt()

    private fun requireNative() {
        val jna = File(System.getProperty("jna.library.path") ?: "").listFiles()?.any { it.name.startsWith("libnavcore_ffi") } == true
        val jni = javaClass.getResource("/darwin-arm64/libnavcore_boltffi_jni.dylib") != null ||
            javaClass.getResource("/darwin-x86_64/libnavcore_boltffi_jni.dylib") != null ||
            javaClass.getResource("/linux-x86_64/libnavcore_boltffi_jni.so") != null
        assumeTrue("both host libraries are needed (cargo build -p navcore-ffi --release; boltffi pack android)", jna && jni)
    }

    private fun routeJson(name: String): String =
        JSONObject(File(fixtures, "$name.json").readText()).getJSONObject("route").toString()

    /** A long straight route with `vertices` points, to show record-size sensitivity. */
    private fun longRouteJson(vertices: Int): String {
        val geometry = StringBuilder("[")
        val steps = StringBuilder("[")
        for (i in 0 until vertices) {
            if (i > 0) geometry.append(',')
            geometry.append("{\"lat\":52.5,\"lng\":${13.4 + i * 0.0003}}")
        }
        val stepLen = 50
        var start = 0
        var first = true
        while (start < vertices - 1) {
            val end = minOf(start + stepLen, vertices - 1)
            if (!first) steps.append(',')
            first = false
            steps.append("{\"instruction\":\"Continue $start\",\"maneuver\":\"${if (start == 0) "Depart" else "Straight"}\",\"start_index\":$start,\"end_index\":$end,\"distance_m\":${(end - start) * 20.0}}")
            start = end
        }
        steps.append(",{\"instruction\":\"Arrive\",\"maneuver\":\"Arrive\",\"start_index\":${vertices - 1},\"end_index\":${vertices - 1},\"distance_m\":0.0}]")
        geometry.append(']')
        return "{\"geometry\":$geometry,\"steps\":$steps}"
    }

    private data class Stats(val avgUs: Double, val p50Us: Double, val p99Us: Double, val maxUs: Double)

    private fun stats(samples: LongArray): Stats {
        samples.sort()
        return Stats(
            avgUs = samples.average() / 1000.0,
            p50Us = samples[samples.size / 2] / 1000.0,
            p99Us = samples[((samples.size - 1) * 0.99).toInt()] / 1000.0,
            maxUs = samples.last() / 1000.0,
        )
    }

    /** Pull: one synchronous call returning the full state per fix. */
    private fun benchPull(routeJson: String): Stats {
        val route = pullRouteFromJson(routeJson)
        val sim = PullSimulator(route, PullSimConfig(speedMps = 5.0))
        val nav = PullNavigator(route, PullConfig())
        val samples = LongArray(calls)
        repeat(200) { nav.updateLocation(sim.nextFix()) } // warm-up (JIT + JNA)
        for (i in 0 until calls) {
            val fix = sim.nextFix()
            val t0 = System.nanoTime()
            nav.updateLocation(fix)
            samples[i] = System.nanoTime() - t0
        }
        nav.destroy()
        sim.destroy()
        return stats(samples)
    }

    /** Push: one call per fix; the position (and sometimes trip) callback runs before it returns. */
    private fun benchPush(routeJson: String): Stats {
        val route = pushRouteFromJson(routeJson)
        val sim = PushSimulator(route, PushSimConfig(speedMps = 5.0))
        val controller = NavigationController(route, PushConfig())
        var positions = 0
        controller.addListener(object : NavigationListener {
            override fun onPosition(position: PositionState) { positions++ }
            override fun onTrip(trip: TripState) {}
            override fun onSession(session: SessionState) {}
            override fun onError(kind: ErrorKind, message: String) {}
        })
        val samples = LongArray(calls)
        repeat(200) { controller.updateLocation(sim.nextFix()) }
        for (i in 0 until calls) {
            val fix = sim.nextFix()
            val t0 = System.nanoTime()
            controller.updateLocation(fix)
            samples[i] = System.nanoTime() - t0
        }
        check(positions == calls + 200)
        controller.close()
        sim.close()
        return stats(samples)
    }

    @Test
    fun boundaryCost() {
        requireNative()
        val cases = listOf(
            "route_simple (${JSONObject(routeJson("route_simple")).getJSONArray("geometry").length()} vertices)" to routeJson("route_simple"),
            "straight 2000 vertices" to longRouteJson(2000),
        )
        val rows = ArrayList<String>()
        val json = StringBuilder("[")
        for ((label, route) in cases) {
            // Interleave so neither approach benefits from a warmer CPU.
            val pull1 = benchPull(route)
            val push1 = benchPush(route)
            val pull2 = benchPull(route)
            val push2 = benchPush(route)
            val pull = if (pull1.p50Us < pull2.p50Us) pull1 else pull2
            val push = if (push1.p50Us < push2.p50Us) push1 else push2
            for ((approach, s) in listOf("pull (UniFFI+JNA)" to pull, "push (BoltFFI+JNI)" to push)) {
                rows += String.format(
                    "| %-26s | %-20s | %8.1f | %8.1f | %8.1f | %8.1f |",
                    label, approach, s.avgUs, s.p50Us, s.p99Us, s.maxUs,
                )
                if (json.length > 1) json.append(',')
                json.append("{\"case\":\"$label\",\"approach\":\"$approach\",\"avg_us\":${s.avgUs},\"p50_us\":${s.p50Us},\"p99_us\":${s.p99Us},\"max_us\":${s.maxUs},\"calls\":$calls}")
            }
        }
        json.append(']')
        val table = buildString {
            appendLine("BOUNDARY BENCH (JVM, $calls calls each, µs per update_location)")
            appendLine("| case                       | approach             |      avg |      p50 |      p99 |      max |")
            appendLine("|----------------------------|----------------------|----------|----------|----------|----------|")
            rows.forEach { appendLine(it) }
        }
        println(table)
        val out = File(System.getProperty("navsdk.bench.out") ?: "build/boundary-bench.json")
        out.parentFile?.mkdirs()
        out.writeText(json.toString())
        File(out.parentFile, "boundary-bench.md").writeText(table)
    }
}
