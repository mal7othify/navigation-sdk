package com.navsdk.demo

import android.Manifest
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AssistChip
import androidx.compose.material3.AssistChipDefaults
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.FilterChip
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Switch
import androidx.compose.material3.Tab
import androidx.compose.material3.TabRow
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.PathEffect
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.drawText
import androidx.compose.ui.text.rememberTextMeasurer
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import java.util.Locale
import com.navsdk.core.TripProgress as PullProgress
import com.navsdk.core.coreVersion as pullCoreVersion
import com.navsdk.push.core.TripProgress as PushProgress
import com.navsdk.push.core.coreVersion as pushCoreVersion

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent {
            MaterialTheme {
                DemoScreen()
            }
        }
    }
}

/** Instruction-card inputs, identical for both approaches so one card composable serves both. */
data class CardTrip(val instruction: String, val stepIndex: Int, val stepCount: Int, val offRoute: Boolean, val arrived: Boolean)
data class CardPosition(
    val toManeuverM: Double,
    val remainingM: Double,
    val speedMps: Double,
    val etaS: Double?,
    val lat: Double,
    val lng: Double,
    val fromRouteM: Double,
    val bearingDeg: Double,
    val offRoute: Boolean,
    val arrived: Boolean,
)

@Composable
fun DemoScreen(vm: DemoViewModel = viewModel()) {
    val approach by vm.approach.collectAsStateWithLifecycle()
    val source by vm.source.collectAsStateWithLifecycle()
    val fixtureName by vm.fixtureName.collectAsStateWithLifecycle()
    val running by vm.running.collectAsStateWithLifecycle()
    val feedIndex by vm.feedIndex.collectAsStateWithLifecycle()
    val feedTotal by vm.feedTotal.collectAsStateWithLifecycle()
    val lastError by vm.lastError.collectAsStateWithLifecycle()
    val deviating by vm.simulatorDeviating.collectAsStateWithLifecycle()

    val permission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        if (granted) vm.setSource(Source.DEVICE)
    }

    Scaffold { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .verticalScroll(rememberScrollState())
                .padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Text(
                "NavSDK demo · pull core ${pullCoreVersion()} · push core ${pushCoreVersion()}",
                style = MaterialTheme.typography.labelMedium,
            )

            TabRow(selectedTabIndex = approach.ordinal) {
                Approach.entries.forEach { tab ->
                    Tab(
                        selected = tab == approach,
                        onClick = { vm.selectApproach(tab) },
                        text = { Text(tab.label) },
                        modifier = Modifier.testTag("tab_${tab.name}"),
                    )
                }
            }

            LazyRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                items(Source.entries) { s ->
                    FilterChip(
                        selected = s == source,
                        onClick = {
                            if (s == Source.DEVICE) permission.launch(Manifest.permission.ACCESS_FINE_LOCATION)
                            else vm.setSource(s)
                        },
                        label = { Text(s.label) },
                    )
                }
            }

            if (source == Source.DEVICE) {
                DeviceControls(vm)
            } else {
                LazyRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    items(Fixture.NAMES) { name ->
                        FilterChip(
                            selected = name == fixtureName,
                            onClick = { vm.selectFixture(name) },
                            label = { Text(name.removePrefix("route_")) },
                        )
                    }
                }
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Button(onClick = { vm.start() }, enabled = !running) { Text("Run 1×") }
                    Button(onClick = { vm.start(10.0) }, enabled = !running) { Text("Run 10×") }
                    OutlinedButton(onClick = { vm.stopFeed() }, enabled = running) { Text("Stop") }
                    if (source == Source.SIMULATOR) {
                        OutlinedButton(onClick = { vm.deviateNow() }, enabled = running && !deviating) { Text("Deviate") }
                    }
                }
                if (feedTotal > 0) {
                    LinearProgressIndicator(progress = { feedIndex.toFloat() / feedTotal }, modifier = Modifier.fillMaxWidth())
                }
                Text("fix $feedIndex${if (feedTotal > 0) " / $feedTotal" else ""}", style = MaterialTheme.typography.labelSmall)
            }

            when (approach) {
                Approach.PULL -> PullPane(vm)
                Approach.PUSH -> PushPane(vm)
                Approach.BOTH -> {
                    DivergenceStrip(vm)
                    PullPane(vm)
                    PushPane(vm)
                }
            }

            lastError?.let {
                Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.labelSmall)
            }
        }
    }
}

/** Device mode: GPS status, walking-course setup, auto-reroute toggle. */
@Composable
private fun DeviceControls(vm: DemoViewModel) {
    val loc by vm.lastDeviceLocation.collectAsStateWithLifecycle()
    val courseLegM by vm.courseLegM.collectAsStateWithLifecycle()
    val autoReroute by vm.autoReroute.collectAsStateWithLifecycle()
    val courseActive by vm.courseActive.collectAsStateWithLifecycle()
    val here = loc
    Text(
        if (here == null) "Waiting for a GPS fix…"
        else String.format(
            Locale.US, "GPS %.5f, %.5f · ±%.0f m · %s",
            here.latitude, here.longitude, here.accuracy,
            if (here.hasBearing()) String.format(Locale.US, "bearing %.0f°", here.bearing) else "no bearing yet",
        ),
        style = MaterialTheme.typography.bodySmall,
    )
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        listOf(30.0, 50.0, 100.0).forEach { leg ->
            FilterChip(selected = courseLegM == leg, onClick = { vm.courseLegM.value = leg }, label = { Text("${leg.toInt()} m") })
        }
    }
    Button(onClick = { vm.startWalkCourse() }, enabled = here != null) { Text("Start walking course here") }
    Row(verticalAlignment = Alignment.CenterVertically) {
        Text("Auto-reroute when the core asks", modifier = Modifier.width(220.dp))
        Switch(checked = autoReroute, onCheckedChange = { vm.autoReroute.value = it })
    }
    Text(
        if (courseActive) "Walk straight, turn right, turn left. Leave the course on purpose to see OFF ROUTE and a reroute: " +
            "the pull wrapper fetches it, the push core fetches it itself."
        else "Walk straight, turn right, turn left. You finish about two legs from where you started; walk back and tap again to repeat.",
        style = MaterialTheme.typography.labelSmall,
    )
}

@Composable
private fun PullPane(vm: DemoViewModel) {
    val position by vm.pullPosition.collectAsStateWithLifecycle()
    val trip by vm.pullTrip.collectAsStateWithLifecycle()
    val metrics by vm.pullMetrics.collectAsStateWithLifecycle()
    val route by vm.activeRoute.collectAsStateWithLifecycle()
    val raw by vm.lastRawPoint.collectAsStateWithLifecycle()
    TripCard(
        title = Approach.PULL.label,
        tag = "card_pull",
        position = position?.let {
            CardPosition(
                it.distanceToNextManeuverM, it.distanceRemainingM, it.speedMps, it.etaS,
                it.snapped.point.lat, it.snapped.point.lng,
                it.snapped.distanceFromRouteM, it.snapped.bearingDeg, it.isOffRoute, it.progress == PullProgress.ARRIVED,
            )
        },
        trip = trip?.let {
            CardTrip(it.nextInstruction, it.currentStepIndex.toInt(), it.stepCount.toInt(), it.isOffRoute, it.progress == PullProgress.ARRIVED)
        },
        metrics = metrics,
        route = route,
        rawPoint = raw,
    )
}

@Composable
private fun PushPane(vm: DemoViewModel) {
    val position by vm.pushPosition.collectAsStateWithLifecycle()
    val trip by vm.pushTrip.collectAsStateWithLifecycle()
    val metrics by vm.pushMetrics.collectAsStateWithLifecycle()
    val route by vm.activeRoute.collectAsStateWithLifecycle()
    val raw by vm.lastRawPoint.collectAsStateWithLifecycle()
    TripCard(
        title = Approach.PUSH.label,
        tag = "card_push",
        position = position?.let {
            CardPosition(
                it.distanceToNextManeuverM, it.distanceRemainingM, it.speedMps, it.etaS,
                it.snapped.point.lat, it.snapped.point.lng,
                it.snapped.distanceFromRouteM, it.snapped.bearingDeg, it.isOffRoute, it.progress == PushProgress.ARRIVED,
            )
        },
        trip = trip?.let {
            CardTrip(it.nextInstruction, it.currentStepIndex.toInt(), it.stepCount.toInt(), it.isOffRoute, it.progress == PushProgress.ARRIVED)
        },
        metrics = metrics,
        route = route,
        rawPoint = raw,
    )
}

@Composable
private fun DivergenceStrip(vm: DemoViewModel) {
    val divergence by vm.divergenceM.collectAsStateWithLifecycle()
    val ever by vm.everDiverged.collectAsStateWithLifecycle()
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        Badge(
            text = divergence?.let { String.format(Locale.US, "snapped Δ %.2f m", it) } ?: "snapped Δ —",
            color = Color(0xFF455A64),
        )
        Badge(if (ever) "DIVERGED" else "identical so far", if (ever) Color(0xFFB00020) else Color(0xFF2E7D32))
    }
}

/**
 * One card per approach. The instruction block is its own composable keyed
 * on [trip] only, so its recomposition counter shows how often the slow
 * state really changed, independent of the per-fix numbers below it.
 */
@Composable
private fun TripCard(
    title: String,
    tag: String,
    position: CardPosition?,
    trip: CardTrip?,
    metrics: ApproachMetrics,
    route: DualRoute?,
    rawPoint: Pair<Double, Double>?,
) {
    Card(modifier = Modifier.fillMaxWidth().testTag(tag)) {
        Column(modifier = Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(title, style = MaterialTheme.typography.labelLarge)
            if (position == null || trip == null) {
                Text("Waiting for the first fix…", style = MaterialTheme.typography.titleMedium)
            } else {
                InstructionBlock(trip)
                Text("in ${formatDistance(position.toManeuverM)}", style = MaterialTheme.typography.titleLarge)
                Spacer(Modifier.height(4.dp))
                Text("Remaining: ${formatDistance(position.remainingM)} · ETA ${formatEta(position.etaS)}")
                Text(String.format(Locale.US, "Speed %.1f km/h", position.speedMps * 3.6))
                Text(
                    String.format(
                        Locale.US, "Snapped %.5f, %.5f · %.0f m from route · heading %.0f°",
                        position.lat, position.lng, position.fromRouteM, position.bearingDeg,
                    ),
                    style = MaterialTheme.typography.bodySmall,
                )
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    if (position.offRoute) Badge("OFF ROUTE", Color(0xFFB00020))
                    if (position.arrived) Badge("ARRIVED", Color(0xFF2E7D32))
                    if (metrics.replanInFlight) Badge("replanning…", Color(0xFF6D4C41))
                }
            }
            MetricsStrip(metrics)
            route?.let {
                CourseMap(
                    route = it,
                    currentStep = trip?.stepIndex,
                    snapped = position?.let { p -> p.lat to p.lng },
                    offRoute = position?.offRoute == true,
                    rawPoint = rawPoint,
                    modifier = Modifier.fillMaxWidth().height(240.dp),
                )
            }
        }
    }
}

@Composable
private fun InstructionBlock(trip: CardTrip) {
    val recompositions = remember { mutableIntStateOf(0) }
    SideEffect { recompositions.intValue += 1 }
    Text(trip.instruction, style = MaterialTheme.typography.headlineSmall)
    Text(
        "Step ${trip.stepIndex + 1} of ${trip.stepCount} · instruction card recomposed ${recompositions.intValue}×",
        style = MaterialTheme.typography.bodySmall,
    )
}

@Composable
private fun MetricsStrip(m: ApproachMetrics) {
    val b = m.boundary
    Text(
        String.format(
            Locale.US,
            "boundary: %d calls · avg %.1f µs · p99 %.1f µs · max %.1f µs",
            b.calls, b.avgUs, b.p99Us, b.maxUs,
        ),
        style = MaterialTheme.typography.bodySmall,
    )
    Text(
        "updates: ${m.positionUpdates} position · ${m.tripUpdates} trip · reroutes ${m.rerouteRequests} · routes installed ${m.routesInstalled}",
        style = MaterialTheme.typography.bodySmall,
    )
}

/**
 * Schematic map: the route in a local metres frame, the current step
 * highlighted, the raw fix and the snapped position. No map tiles needed.
 * Takes plain coordinates so the same map serves both binding layers.
 */
@Composable
private fun CourseMap(
    route: DualRoute,
    currentStep: Int?,
    snapped: Pair<Double, Double>?,
    offRoute: Boolean,
    rawPoint: Pair<Double, Double>?,
    modifier: Modifier = Modifier,
) {
    val measurer = rememberTextMeasurer()
    val labelStyle = TextStyle(fontSize = 10.sp, color = MaterialTheme.colorScheme.onSurface)
    val fg = MaterialTheme.colorScheme.onSurface
    val bg = MaterialTheme.colorScheme.surfaceVariant
    Canvas(modifier = modifier.clip(RoundedCornerShape(12.dp)).background(bg)) {
        val pts = route.points
        val origin = pts.firstOrNull() ?: return@Canvas
        val mPerDegLat = 110_540.0
        val mPerDegLng = 111_320.0 * Math.cos(Math.toRadians(origin.first))
        fun enu(p: Pair<Double, Double>) =
            Offset(((p.second - origin.second) * mPerDegLng).toFloat(), ((p.first - origin.first) * mPerDegLat).toFloat())
        val all = pts.map(::enu).toMutableList()
        rawPoint?.let { all += enu(it) }
        snapped?.let { all += enu(it) }
        val minX = all.minOf { it.x }; val maxX = all.maxOf { it.x }
        val minY = all.minOf { it.y }; val maxY = all.maxOf { it.y }
        val span = maxOf(maxX - minX, maxY - minY, 20f)
        val pad = 24.dp.toPx()
        val k = (minOf(size.width, size.height) - 2 * pad) / span
        val cx = (minX + maxX) / 2; val cy = (minY + maxY) / 2
        fun screen(e: Offset) = Offset(size.width / 2 + (e.x - cx) * k, size.height / 2 - (e.y - cy) * k)
        fun polyline(points: List<Offset>) = Path().apply {
            points.forEachIndexed { i, p -> if (i == 0) moveTo(p.x, p.y) else lineTo(p.x, p.y) }
        }

        drawPath(polyline(pts.map { screen(enu(it)) }), Color.Gray.copy(alpha = 0.5f), style = Stroke(3.dp.toPx(), join = StrokeJoin.Round))

        currentStep?.let { idx ->
            val range = route.stepRanges.getOrNull(idx) ?: return@let
            if (range.last > range.first) {
                drawPath(polyline(pts.subList(range.first, range.last + 1).map { screen(enu(it)) }), Color(0xFF1D5BBF), style = Stroke(5.dp.toPx(), join = StrokeJoin.Round))
            }
        }

        route.stepRanges.forEachIndexed { i, range ->
            val p = screen(enu(pts[range.first]))
            if (i == route.stepRanges.lastIndex) {
                val r = 7.dp.toPx()
                drawRect(Color(0xFF2E7D32), topLeft = Offset(p.x - r, p.y - r), size = Size(2 * r, 2 * r))
            } else {
                drawCircle(Color.White, 5.dp.toPx(), p)
                drawCircle(Color.Gray, 5.dp.toPx(), p, style = Stroke(2.dp.toPx()))
            }
        }

        rawPoint?.let { raw ->
            val p = screen(enu(raw))
            drawCircle(Color(0xFFB00020), 6.dp.toPx(), p, style = Stroke(2.dp.toPx()))
            snapped?.let {
                val q = screen(enu(it))
                drawLine(Color(0x99B00020), p, q, strokeWidth = 1.dp.toPx(), pathEffect = PathEffect.dashPathEffect(floatArrayOf(6f, 6f)))
            }
        }
        snapped?.let {
            drawCircle(if (offRoute) Color(0xFFB00020) else Color(0xFF1D5BBF), 7.dp.toPx(), screen(enu(it)))
        }

        drawText(measurer, "N", Offset(size.width - 18.dp.toPx(), 4.dp.toPx()), labelStyle)
        drawLine(fg, Offset(size.width - 14.dp.toPx(), 32.dp.toPx()), Offset(size.width - 14.dp.toPx(), 20.dp.toPx()), strokeWidth = 1.5.dp.toPx(), cap = StrokeCap.Round)
        val barY = size.height - 12.dp.toPx()
        drawLine(fg, Offset(12.dp.toPx(), barY), Offset(12.dp.toPx() + 10 * k, barY), strokeWidth = 2.dp.toPx())
        drawText(measurer, "10 m", Offset(12.dp.toPx(), barY - 16.dp.toPx()), labelStyle)
    }
}

@Composable
private fun Badge(text: String, color: Color) {
    AssistChip(
        onClick = {},
        label = { Text(text, color = Color.White) },
        colors = AssistChipDefaults.assistChipColors(containerColor = color),
    )
}

private fun formatDistance(m: Double): String =
    if (m >= 1000) String.format(Locale.US, "%.1f km", m / 1000) else String.format(Locale.US, "%.0f m", m)

/** `null` until the core has seen a few seconds of movement. */
private fun formatEta(s: Double?): String = when {
    s == null -> "—"
    s < 60 -> String.format(Locale.US, "%.0f s", s)
    s < 3600 -> String.format(Locale.US, "%.0f min", s / 60)
    else -> String.format(Locale.US, "%.0f h %02.0f min", Math.floor(s / 3600), (s / 60) % 60)
}
