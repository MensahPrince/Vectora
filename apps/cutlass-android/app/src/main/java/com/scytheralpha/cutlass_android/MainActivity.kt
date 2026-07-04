package com.scytheralpha.cutlass_android

import android.graphics.Bitmap
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.unit.dp
import com.scytheralpha.cutlass_android.ui.theme.CutlassandroidTheme
import org.json.JSONObject
import kotlin.concurrent.thread

/**
 * The engine smoke test: open a session, add a solid clip through the same
 * wire commands the editors send, read `ui_state` back, and render frame 0 on
 * the device GPU. Every step reports into the on-screen log, so a failure
 * names the layer that broke (JNI load, engine, wire protocol, GPU).
 */
class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        setContent {
            CutlassandroidTheme {
                val log = remember { mutableStateOf("running…") }
                val frame = remember { mutableStateOf<Bitmap?>(null) }
                remember {
                    thread {
                        val (text, bitmap) = engineSmokeTest()
                        log.value = text
                        frame.value = bitmap
                    }
                }
                Scaffold(modifier = Modifier.fillMaxSize()) { innerPadding ->
                    val bitmap by frame
                    val text by log
                    Column(
                        modifier = Modifier
                            .padding(innerPadding)
                            .padding(16.dp)
                            .verticalScroll(rememberScrollState())
                    ) {
                        bitmap?.let {
                            Image(
                                bitmap = it.asImageBitmap(),
                                contentDescription = "engine frame",
                                modifier = Modifier.fillMaxWidth(),
                            )
                        }
                        Text(text = text)
                    }
                }
            }
        }
    }
}

/** Run the session round trip; a human-readable transcript plus frame 0. */
private fun engineSmokeTest(): Pair<String, Bitmap?> {
    val log = StringBuilder()
    fun step(name: String, detail: String) = log.append("• ").append(name).append(": ").append(detail).append('\n')

    val catalogs = CutlassNative.catalogs()
    val filterCount = catalogs?.let { JSONObject(it).getJSONArray("filters").length() } ?: 0
    step("catalogs", "$filterCount filters")

    val session = CutlassNative.sessionNew(30, 1)
    if (session == 0L) {
        step("session", "FAILED to open (GPU?)")
        return log.toString() to null
    }
    step("session", "open @30fps")

    try {
        // Response envelope: {"ok": {"type": "Edited", "value": {…, "id": n}}, "revision": n}.
        val track = JSONObject(
            CutlassNative.sessionApply(
                session, """{"type": "AddTrack", "kind": "Sticker", "name": "Solids"}"""
            ) ?: "{}"
        )
        val trackId = track.optJSONObject("ok")?.optJSONObject("value")?.optLong("id", -1)
            ?.takeIf { it >= 0 }
            ?: return log.append("AddTrack FAILED: $track").toString() to null
        step("AddTrack", "sticker lane id=$trackId")

        val added = JSONObject(
            CutlassNative.sessionApply(
                session,
                """
                {"type": "AddGenerated", "track": $trackId,
                 "generator": {"SolidColor": {"rgba": [235, 78, 58, 255]}},
                 "timeline": {"start": {"value": 0, "rate": {"num": 30, "den": 1}},
                              "duration": {"value": 90, "rate": {"num": 30, "den": 1}}}}
                """.trimIndent()
            ) ?: "{}"
        )
        if (!added.has("ok")) return log.append("AddGenerated FAILED: $added").toString() to null
        step("AddGenerated", "3s solid clip")

        val state = JSONObject(CutlassNative.sessionUiState(session) ?: "{}")
        val lanes = state.getJSONArray("lanes")
        val duration = state.optDouble("duration_seconds", 0.0)
        step("ui_state", "${lanes.length()} lanes, ${duration}s, rev=${CutlassNative.sessionRevision(session)}")

        val undo = CutlassNative.sessionUndo(session) && CutlassNative.sessionRedo(session)
        step("undo/redo", if (undo) "round-tripped" else "FAILED")

        val pixels = CutlassNative.sessionRenderFit(session, 0.0, 640, 640)
        if (pixels.size < 2 + 1) return log.append("render FAILED (empty frame)").toString() to null
        val width = pixels[0]
        val height = pixels[1]
        step("render_fit", "${width}x$height frame 0")

        val bitmap = Bitmap.createBitmap(width, height, Bitmap.Config.ARGB_8888)
        bitmap.setPixels(pixels, 2, width, 0, 0, width, height)
        log.append("engine round trip OK")
        return log.toString() to bitmap
    } finally {
        CutlassNative.sessionClose(session)
    }
}
