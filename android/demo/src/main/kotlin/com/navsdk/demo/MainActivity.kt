package com.navsdk.demo

import android.Manifest
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
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
import androidx.compose.material3.Tab
import androidx.compose.material3.TabRow
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
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
                    // Device location feeds a single session; Both mode needs lockstep replay.
                    val enabled = s != Source.DEVICE || approach != Approach.BOTH
                    FilterChip(
                        selected = s == source,
                        enabled = enabled,
                        onClick = {
                            if (s == Source.DEVICE) permission.launch(Manifest.permission.ACCESS_FINE_LOCATION)
                            else vm.setSource(s)
                        },
                        label = { Text(s.label) },
                    )
                }
            }

            if (source != Source.DEVICE) {
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
                Text("last rejected fix: $it", color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.labelSmall)
            }
        }
    }
}

@Composable
private fun PullPane(vm: DemoViewModel) {
    val position by vm.pullPosition.collectAsStateWithLifecycle()
    val trip by vm.pullTrip.collectAsStateWithLifecycle()
    val metrics by vm.pullMetrics.collectAsStateWithLifecycle()
    TripCard(
        title = Approach.PULL.label,
        tag = "card_pull",
        position = position?.let {
            CardPosition(
                it.distanceToNextManeuverM, it.distanceRemainingM, it.snapped.point.lat, it.snapped.point.lng,
                it.snapped.distanceFromRouteM, it.snapped.bearingDeg, it.isOffRoute, it.progress == PullProgress.ARRIVED,
            )
        },
        trip = trip?.let {
            CardTrip(it.nextInstruction, it.currentStepIndex.toInt(), it.stepCount.toInt(), it.isOffRoute, it.progress == PullProgress.ARRIVED)
        },
        metrics = metrics,
    )
}

@Composable
private fun PushPane(vm: DemoViewModel) {
    val position by vm.pushPosition.collectAsStateWithLifecycle()
    val trip by vm.pushTrip.collectAsStateWithLifecycle()
    val metrics by vm.pushMetrics.collectAsStateWithLifecycle()
    TripCard(
        title = Approach.PUSH.label,
        tag = "card_push",
        position = position?.let {
            CardPosition(
                it.distanceToNextManeuverM, it.distanceRemainingM, it.snapped.point.lat, it.snapped.point.lng,
                it.snapped.distanceFromRouteM, it.snapped.bearingDeg, it.isOffRoute, it.progress == PushProgress.ARRIVED,
            )
        },
        trip = trip?.let {
            CardTrip(it.nextInstruction, it.currentStepIndex.toInt(), it.stepCount.toInt(), it.isOffRoute, it.progress == PushProgress.ARRIVED)
        },
        metrics = metrics,
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
private fun TripCard(title: String, tag: String, position: CardPosition?, trip: CardTrip?, metrics: ApproachMetrics) {
    Card(modifier = Modifier.fillMaxWidth().testTag(tag)) {
        Column(modifier = Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(title, style = MaterialTheme.typography.labelLarge)
            if (position == null || trip == null) {
                Text("Waiting for the first fix…", style = MaterialTheme.typography.titleMedium)
            } else {
                InstructionBlock(trip)
                Text("in ${formatDistance(position.toManeuverM)}", style = MaterialTheme.typography.titleLarge)
                Spacer(Modifier.height(4.dp))
                Text("Remaining: ${formatDistance(position.remainingM)}")
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
