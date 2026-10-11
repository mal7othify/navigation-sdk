package com.navsdk.demo

import android.annotation.SuppressLint
import android.app.Application
import android.os.Looper
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import com.google.android.gms.location.LocationCallback
import com.google.android.gms.location.LocationRequest
import com.google.android.gms.location.LocationResult
import com.google.android.gms.location.LocationServices
import com.google.android.gms.location.Priority
import com.navsdk.NavigationSession
import com.navsdk.push.NavigationSessionPush
import com.navsdk.push.core.PlannedDeviation
import com.navsdk.push.core.RouteSimulator
import com.navsdk.push.core.SimulatorConfig
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.flatMapLatest
import kotlinx.coroutines.flow.flowOf
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import com.navsdk.core.PositionState as PullPosition
import com.navsdk.core.TripState as PullTrip
import com.navsdk.push.core.PositionState as PushPosition
import com.navsdk.push.core.TripState as PushTrip

/** Which implementation the screen is driving. */
enum class Approach(val label: String) { PULL("Pull · UniFFI"), PUSH("Push · BoltFFI"), BOTH("Both") }

/** Where fixes come from. */
enum class Source(val label: String) { REPLAY("Replay fixture"), SIMULATOR("Rust simulator"), DEVICE("Device") }

private const val METRICS_WINDOW = 1024
private const val REPLAN_LATENCY_MS = 300L
private const val DIVERGENCE_THRESHOLD_M = 0.5

/**
 * Drives one or two sessions over the same fixes and aggregates the metrics
 * the comparison needs. Pull and push sessions are always created together
 * so the "Both" tab can be switched to without losing state.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class DemoViewModel(app: Application) : AndroidViewModel(app) {
    private val pull = MutableStateFlow<NavigationSession?>(null)
    private val push = MutableStateFlow<NavigationSessionPush?>(null)
    private var fixture: Fixture? = null
    private var simulator: RouteSimulator? = null
    private var feedJob: Job? = null

    val approach = MutableStateFlow(Approach.PULL)
    val source = MutableStateFlow(Source.REPLAY)
    val fixtureName = MutableStateFlow(Fixture.NAMES.first())
    val running = MutableStateFlow(false)
    val feedIndex = MutableStateFlow(0)
    val feedTotal = MutableStateFlow(0)
    val lastError = MutableStateFlow<String?>(null)
    val simulatorDeviating = MutableStateFlow(false)

    // ---- Pull (Approach A)
    val pullPosition: StateFlow<PullPosition?> = pull
        .flatMapLatest { it?.position ?: flowOf(null) }
        .stateIn(viewModelScope, SharingStarted.Eagerly, null)
    val pullTrip: StateFlow<PullTrip?> = pull
        .flatMapLatest { it?.trip ?: flowOf(null) }
        .stateIn(viewModelScope, SharingStarted.Eagerly, null)
    private val pullSamples = ArrayDeque<Long>()
    private val _pullMetrics = MutableStateFlow(ApproachMetrics())
    val pullMetrics: StateFlow<ApproachMetrics> = _pullMetrics.asStateFlow()

    // ---- Push (Approach B)
    val pushPosition: StateFlow<PushPosition?> = push
        .flatMapLatest { it?.position ?: flowOf(null) }
        .stateIn(viewModelScope, SharingStarted.Eagerly, null)
    val pushTrip: StateFlow<PushTrip?> = push
        .flatMapLatest { it?.trip ?: flowOf(null) }
        .stateIn(viewModelScope, SharingStarted.Eagerly, null)
    private val pushSamples = ArrayDeque<Long>()
    private val _pushMetrics = MutableStateFlow(ApproachMetrics())
    val pushMetrics: StateFlow<ApproachMetrics> = _pushMetrics.asStateFlow()

    /** Both mode: metres between the two snapped positions, and whether they ever diverged. */
    val divergenceM: StateFlow<Double?> = combine(pullPosition, pushPosition) { a, b ->
        if (a == null || b == null) null
        else haversineM(a.snapped.point.lat, a.snapped.point.lng, b.snapped.point.lat, b.snapped.point.lng)
    }.stateIn(viewModelScope, SharingStarted.Eagerly, null)
    val everDiverged = MutableStateFlow(false)

    private val fused = LocationServices.getFusedLocationProviderClient(app)
    private val locationCallback = object : LocationCallback() {
        override fun onLocationResult(result: LocationResult) {
            val location = result.lastLocation ?: return
            if (approach.value != Approach.PUSH) pull.value?.update(location)
            if (approach.value != Approach.PULL) push.value?.update(location)
        }
    }

    init {
        viewModelScope.launch {
            divergenceM.collect { d -> if (d != null && d > DIVERGENCE_THRESHOLD_M) everDiverged.value = true }
        }
        selectFixture(fixtureName.value)
    }

    fun selectApproach(next: Approach) {
        approach.value = next
    }

    fun selectFixture(name: String) {
        stopFeed()
        fixtureName.value = name
        viewModelScope.launch {
            val loaded = withContext(Dispatchers.IO) { Fixture.load(getApplication<Application>().assets, name) }
            fixture = loaded
            feedTotal.value = loaded.size
            feedIndex.value = 0
            resetSessions(loaded)
        }
    }

    /** Fresh sessions and metrics for both approaches. */
    private fun resetSessions(loaded: Fixture) {
        pull.value?.close()
        push.value?.close()
        simulator?.close()
        simulator = RouteSimulator(loaded.pushRoute, SimulatorConfig())
        simulatorDeviating.value = false
        everDiverged.value = false
        pullSamples.clear()
        pushSamples.clear()
        _pullMetrics.value = ApproachMetrics()
        _pushMetrics.value = ApproachMetrics()

        // Pull: host-driven rerouting. The demo has no routing backend, so the
        // "fetched" route is the same route after a short delay.
        val a = NavigationSession(loaded.pullRoute, replanner = { _ ->
            delay(REPLAN_LATENCY_MS)
            loaded.pullRoute
        })
        // Push: the core calls this from its helper thread and installs the result itself.
        val b = NavigationSessionPush(loaded.pushRoute, replanner = { _ ->
            delay(REPLAN_LATENCY_MS)
            loaded.pushRoute
        })
        pull.value = a
        push.value = b

        viewModelScope.launch { a.errors.collect { lastError.value = "pull: ${it.message}" } }
        viewModelScope.launch { b.errors.collect { lastError.value = "push: ${it.message}" } }
        viewModelScope.launch { a.callNanos.collect { pullSamples.push(it); refreshPull(a) } }
        viewModelScope.launch { b.callNanos.collect { pushSamples.push(it); refreshPush(b) } }
        viewModelScope.launch {
            combine(a.positionUpdates, a.tripUpdates, a.rerouteRequests, a.routesInstalled) { _, _, _, _ -> }
                .collect { refreshPull(a) }
        }
        viewModelScope.launch {
            combine(b.positionCallbacks, b.tripCallbacks, b.rerouteRequests, b.session) { _, _, _, _ -> }
                .collect { refreshPush(b) }
        }
    }

    private fun ArrayDeque<Long>.push(sample: Long) {
        addLast(sample)
        if (size > METRICS_WINDOW) removeFirst()
    }

    private fun refreshPull(a: NavigationSession) {
        _pullMetrics.update {
            ApproachMetrics(
                boundary = BoundaryStats.of(pullSamples.toList(), a.positionUpdates.value),
                positionUpdates = a.positionUpdates.value,
                tripUpdates = a.tripUpdates.value,
                rerouteRequests = a.rerouteRequests.value,
                routesInstalled = a.routesInstalled.value,
            )
        }
    }

    private fun refreshPush(b: NavigationSessionPush) {
        val session = b.session.value
        _pushMetrics.update {
            ApproachMetrics(
                boundary = BoundaryStats.of(pushSamples.toList(), b.positionCallbacks.value),
                positionUpdates = b.positionCallbacks.value,
                tripUpdates = b.tripCallbacks.value,
                rerouteRequests = b.rerouteRequests.value,
                routesInstalled = session.routesInstalled.toInt(),
                replanInFlight = session.replanInFlight,
            )
        }
    }

    /** Feed the chosen source at 1 Hz × [speed] to the session(s) of the current tab. */
    fun start(speed: Double = 1.0) {
        val loaded = fixture ?: return
        stopFeed()
        resetSessions(loaded)
        running.value = true
        val tab = approach.value
        feedJob = viewModelScope.launch {
            when (source.value) {
                Source.REPLAY -> {
                    for (i in 0 until loaded.size) {
                        feedOne(tab, loaded, i)
                        feedIndex.value = i + 1
                        delay((1000.0 / speed).toLong())
                    }
                }
                Source.SIMULATOR -> {
                    val sim = simulator ?: return@launch
                    var n = 0
                    var tail = 5
                    feedTotal.value = 0
                    while (tail > 0) {
                        val fix = sim.nextFix()
                        if (tab != Approach.PUSH) pull.value?.update(fix.toPull())
                        if (tab != Approach.PULL) push.value?.update(fix)
                        feedIndex.value = ++n
                        if (sim.isFinished()) tail -= 1
                        delay((1000.0 / speed).toLong())
                    }
                }
                Source.DEVICE -> return@launch
            }
            running.value = false
        }
    }

    private suspend fun feedOne(tab: Approach, loaded: Fixture, i: Int) {
        when (tab) {
            Approach.PULL -> pull.value?.update(loaded.pullTrace[i])
            Approach.PUSH -> push.value?.update(loaded.pushTrace[i])
            // Lockstep: wait for each so the divergence check compares the same fix.
            Approach.BOTH -> {
                runCatching { pull.value?.updateAndAwait(loaded.pullTrace[i]) }
                push.value?.updateAndAwait(loaded.pushTrace[i])
            }
        }
    }

    /** Simulator only: leave the route 50 m ahead for 300 m, 80 m to the right. */
    fun deviateNow() {
        val sim = simulator ?: return
        sim.setPlannedDeviation(PlannedDeviation(atM = sim.distanceAlongM() + 50.0, lateralM = 80.0, lengthM = 300.0))
        simulatorDeviating.value = true
    }

    fun stopFeed() {
        feedJob?.cancel()
        feedJob = null
        running.value = false
    }

    /** Switch the fix source. Device location needs the permission granted first. */
    @SuppressLint("MissingPermission") // The Activity requests permission before calling.
    fun setSource(next: Source) {
        if (source.value == next) return
        source.value = next
        stopFeed()
        fixture?.let { resetSessions(it) }
        if (next == Source.DEVICE) {
            val request = LocationRequest.Builder(Priority.PRIORITY_HIGH_ACCURACY, 1000L).build()
            fused.requestLocationUpdates(request, locationCallback, Looper.getMainLooper())
        } else {
            fused.removeLocationUpdates(locationCallback)
        }
    }

    override fun onCleared() {
        stopFeed()
        fused.removeLocationUpdates(locationCallback)
        pull.value?.close()
        push.value?.close()
        simulator?.close()
    }
}
