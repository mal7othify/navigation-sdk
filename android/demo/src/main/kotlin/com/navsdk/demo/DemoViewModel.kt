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
import com.navsdk.core.TripState
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
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

    val state: StateFlow<TripState?> = session
        .flatMapLatest { it?.state ?: flowOf(null) }
        .stateIn(viewModelScope, SharingStarted.Eagerly, null)

    val rerouteRequests: StateFlow<Int> = session
        .flatMapLatest { it?.rerouteRequests ?: flowOf(0) }
        .stateIn(viewModelScope, SharingStarted.Eagerly, 0)

    private val fused = LocationServices.getFusedLocationProviderClient(app)
    private val locationCallback = object : LocationCallback() {
        override fun onLocationResult(result: LocationResult) {
            result.lastLocation?.let { session.value?.update(it) }
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
            stepCount.value = loaded.route.steps.size
            resetSession(loaded)
        }
    }

    private fun resetSession(loaded: Fixture) {
        session.value?.close()
        val next = NavigationSession(loaded.route)
        session.value = next
        viewModelScope.launch {
            next.errors.collect { lastError.value = it.message }
        }
    }

    /** Replay the fixture trace at 1 Hz × [speed]. */
    fun startReplay(speed: Double = 1.0) {
        val loaded = fixture ?: return
        stopReplay()
        resetSession(loaded)
        replaying.value = true
        replayJob = viewModelScope.launch {
            for ((i, raw) in loaded.trace.withIndex()) {
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
        fixture?.let { resetSession(it) }
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
