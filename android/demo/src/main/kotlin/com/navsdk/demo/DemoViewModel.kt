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
import android.location.Location
import com.navsdk.NavigationSession
import com.navsdk.core.GeoPoint
import com.navsdk.core.Route
import com.navsdk.core.TripState
import com.navsdk.core.directRoute
import com.navsdk.core.walkCourse
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.drop
import kotlinx.coroutines.flow.flatMapLatest
import kotlinx.coroutines.flow.flowOf
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

enum class Source { REPLAY, DEVICE }

@OptIn(ExperimentalCoroutinesApi::class)
class DemoViewModel(app: Application) : AndroidViewModel(app) {
    private val session = MutableStateFlow<NavigationSession?>(null)
    private var fixture: Fixture? = null
    private var replayJob: Job? = null

    val fixtureName = MutableStateFlow(Fixture.NAMES.first())
    val source = MutableStateFlow(Source.REPLAY)
    val replaying = MutableStateFlow(false)
    val replayIndex = MutableStateFlow(0)
    val replayTotal = MutableStateFlow(0)
    val stepCount = MutableStateFlow(0)
    val lastError = MutableStateFlow<String?>(null)

    /** Route the session is navigating; drawn by the schematic map. */
    val activeRoute = MutableStateFlow<Route?>(null)
    /** Most recent raw fix handed to the core (replayed or from the device). */
    val lastRawPoint = MutableStateFlow<GeoPoint?>(null)
    /** Most recent device fix, for anchoring a walking course. */
    val lastDeviceLocation = MutableStateFlow<Location?>(null)
    /** Leg length of the walking course, metres. */
    val courseLegM = MutableStateFlow(50.0)
    /** When the core asks for a reroute, install a straight line to the destination. */
    val autoReroute = MutableStateFlow(true)
    val reroutesInstalled = MutableStateFlow(0)
    /** True while the session is navigating a walking course started from the device position. */
    val courseActive = MutableStateFlow(false)
    private var destination: GeoPoint? = null
    private var sessionJobs: Job? = null

    val state: StateFlow<TripState?> = session
        .flatMapLatest { it?.state ?: flowOf(null) }
        .stateIn(viewModelScope, SharingStarted.Eagerly, null)

    val rerouteRequests: StateFlow<Int> = session
        .flatMapLatest { it?.rerouteRequests ?: flowOf(0) }
        .stateIn(viewModelScope, SharingStarted.Eagerly, 0)

    private val fused = LocationServices.getFusedLocationProviderClient(app)
    private val locationCallback = object : LocationCallback() {
        override fun onLocationResult(result: LocationResult) {
            val loc = result.lastLocation ?: return
            lastDeviceLocation.value = loc
            lastRawPoint.value = GeoPoint(loc.latitude, loc.longitude)
            session.value?.update(loc)
        }
    }

    init {
        selectFixture(fixtureName.value)
    }

    fun selectFixture(name: String) {
        stopReplay()
        fixtureName.value = name
        viewModelScope.launch {
            val loaded = withContext(Dispatchers.IO) {
                Fixture.load(getApplication<Application>().assets, name)
            }
            fixture = loaded
            replayTotal.value = loaded.trace.size
            replayIndex.value = 0
            startSession(loaded.route)
        }
    }

    /** Replace the session with one navigating [route]. */
    private fun startSession(route: Route, course: Boolean = false) {
        sessionJobs?.cancel()
        session.value?.close()
        val next = NavigationSession(route)
        session.value = next
        activeRoute.value = route
        stepCount.value = route.steps.size
        destination = route.geometry.lastOrNull()
        courseActive.value = course
        reroutesInstalled.value = 0
        lastRawPoint.value = null
        lastError.value = null
        sessionJobs = viewModelScope.launch {
            launch { next.errors.collect { lastError.value = "rejected fix: ${it.message}" } }
            // Each reroute request the core issues installs a straight-line
            // route from the current position to the original destination.
            launch {
                next.rerouteRequests.drop(1).collect { handleRerouteRequest(next) }
            }
        }
    }

    /**
     * Only meaningful while walking a course: a fixture route in Device mode
     * is just a placeholder and the device is nowhere near it.
     */
    private suspend fun handleRerouteRequest(current: NavigationSession) {
        if (!autoReroute.value || !courseActive.value) return
        val to = destination ?: return
        val here = lastDeviceLocation.value ?: return
        try {
            // Route building and the Rust call stay off the main thread.
            val route = withContext(Dispatchers.Default) {
                directRoute(GeoPoint(here.latitude, here.longitude), to)
            }
            current.setRoute(route)
            activeRoute.value = route
            stepCount.value = route.steps.size
            reroutesInstalled.value += 1
        } catch (e: CancellationException) {
            throw e // a newer session replaced this one; not an error
        } catch (e: Exception) {
            lastError.value = "reroute failed: ${e.message}"
        }
    }

    /**
     * Lay out a square walking course at the current device position, facing
     * the way the device is moving (north if it has no bearing yet).
     */
    fun startWalkCourse() {
        val here = lastDeviceLocation.value ?: return
        val heading = if (here.hasBearing()) here.bearing.toDouble() else 0.0
        try {
            val route = walkCourse(GeoPoint(here.latitude, here.longitude), heading, courseLegM.value)
            startSession(route, course = true)
        } catch (e: Exception) {
            lastError.value = e.message
        }
    }

    /** Replay the fixture trace at 1 Hz × [speed]. */
    fun startReplay(speed: Double = 1.0) {
        val loaded = fixture ?: return
        stopReplay()
        startSession(loaded.route)
        replaying.value = true
        replayJob = viewModelScope.launch {
            for ((i, raw) in loaded.trace.withIndex()) {
                lastRawPoint.value = raw.point
                session.value?.update(raw)
                replayIndex.value = i + 1
                delay((1000.0 / speed).toLong())
            }
            replaying.value = false
        }
    }

    fun stopReplay() {
        replayJob?.cancel()
        replayJob = null
        replaying.value = false
    }

    /** Switch between fixture replay and the device's fused location provider. */
    @SuppressLint("MissingPermission") // The Activity requests permission before calling.
    fun setSource(next: Source) {
        if (source.value == next) return
        source.value = next
        stopReplay()
        fixture?.let { startSession(it.route) }
        if (next == Source.DEVICE) {
            val request = LocationRequest.Builder(Priority.PRIORITY_HIGH_ACCURACY, 1000L).build()
            fused.requestLocationUpdates(request, locationCallback, Looper.getMainLooper())
        } else {
            fused.removeLocationUpdates(locationCallback)
        }
    }

    override fun onCleared() {
        stopReplay()
        fused.removeLocationUpdates(locationCallback)
        session.value?.close()
    }
}
