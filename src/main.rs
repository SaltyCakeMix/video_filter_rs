use std::time::Instant;

use ffmpeg_the_third::{codec::{self, Parameters}, decoder, encoder, ffi::AV_PKT_FLAG_KEY, format::{self, Pixel}, frame, media, packet, software::scaling::{Context, Flags}, Dictionary, Packet, Rational};
use rusttype::{point, Font, Scale};

struct RenderData {
    f_w: usize,
    _f_h: usize,
    r_w: usize,
    r_h: usize,
    x: Vec<usize>,
    y: Vec<usize>,
}

impl RenderData {
    fn new(r_w: u32, r_h: u32, dst_w: u32, dst_h: u32, f_w: u32, f_h: u32) -> Self {
        Self {
            f_w: f_w as usize,
            _f_h: f_h as usize,
            r_w: r_w as usize,
            r_h: r_h as usize,
            x: (0..r_w).map(|x| (x as f32 * dst_w as f32 / r_w as f32) as usize).collect(),
            y: (0..r_h).map(|x| (x as f32 * dst_h as f32 / r_h as f32) as usize).collect(),
        }
    }
}

fn decode_frames(decoder: &mut decoder::Video, encoder: &mut encoder::Video, scaler: &mut Context, char_set: &[Vec<Vec<u8>>], render_data: &RenderData, template: &frame::Video) {
    let mut frame = frame::Video::empty();
    let lum_to_char = char_set.len() as f32 / 256.;
    while decoder.receive_frame(&mut frame).is_ok() {
        // Scale frame to render resolution
        let mut scaled_frame = frame::Video::new(Pixel::GRAY8, render_data.r_w as u32, render_data.r_h as u32);
        scaler.run(&frame, &mut scaled_frame).unwrap();
        let padding = scaled_frame.stride(0) - render_data.r_w;
        let luminosity = scaled_frame.data_mut(0);

        // Render characters on to output frame
        let mut new_frame = template.clone();
        let bytes = new_frame.data_mut(0);
        let mut i = 0;
        for y in render_data.y.iter() {
            for x in render_data.x.iter() {
                let char_idx = luminosity[i] as f32 * lum_to_char;
                let stamp = &char_set[char_idx as usize];

                let mut start = x + y*template.stride(0);
                for line in stamp {
                    bytes[start..(start + render_data.f_w)].copy_from_slice(line);
                    start += template.stride(0);
                }
                i += 1;
            }
            i+= padding;
        }

        new_frame.set_pts(frame.timestamp());
        encoder.send_frame(&new_frame).unwrap();
    }
}

fn encode_frames(encoder: &mut encoder::Video, out_vid_stream_idx: usize, in_vid_tb: Rational, out_vid_tb: Rational, out_ctx: &mut format::context::Output, frame_ct: &mut u32) {
    let mut encoded = Packet::empty();
    while encoder.receive_packet(&mut encoded).is_ok() {
        encoded.set_stream(out_vid_stream_idx);
        encoded.rescale_ts(in_vid_tb, out_vid_tb);
        encoded.set_flags(packet::Flags::from_bits(AV_PKT_FLAG_KEY).unwrap());
        encoded.write_interleaved(out_ctx).unwrap();
        *frame_ct += 1;
    } 
}

fn construct_char_set(font_path: &[u8], chars: &str, font_h: u32) -> (u32, Vec<Vec<Vec<u8>>>) {
    // Loads font
    let font = Font::try_from_bytes(font_path).expect("Error constructing Font");

    // Determines proper font scaling
    let value_cutoff = 0.5;
    let func = |height| {
        let scale = Scale::uniform(height);
        let v_metrics = font.v_metrics(scale);
        let glyphs: Vec<_> = font.layout(chars, scale, point(0., v_metrics.ascent)).collect();
        let (glyphs_width, glyphs_height) = {
            let mut top = glyphs
                .iter()
                .map(|g| g.pixel_bounding_box().unwrap_or_default().height())
                .max()
                .unwrap() as u32;
            let mut left = glyphs
                .iter()
                .map(|g| g.pixel_bounding_box().unwrap_or_default().width())
                .max()
                .unwrap() as u32;
            let mut bottom = 0;
            let mut right = 0;
            for glyph in glyphs.iter() {
                glyph.draw(|x, y, v| {
                    if v > value_cutoff {
                        top = y.min(top);
                        bottom = y.max(bottom);
                        left = x.min(left);
                        right = x.max(right);
                    }
                });
            }
            (right - left, bottom - top)
        };
        (glyphs, glyphs_width, glyphs_height)
    };
    let (_, _, glyphs_height) = func(font_h as f32);

    let mut adj_font_h: f32 = font_h as f32 * font_h as f32 / glyphs_height as f32;
    let (mut glyphs, mut glyphs_width, mut glyphs_height) = func(adj_font_h);
    while glyphs_height > font_h {
        adj_font_h *= font_h as f32 / glyphs_height as f32;
        (glyphs, glyphs_width, glyphs_height) = func(adj_font_h);
    }
    // Render characters
    let mut glyph_bytes: Vec<Vec<Vec<u8>>> = Vec::with_capacity(glyphs.len());
    let scale = Scale::uniform(adj_font_h);
    let v_metrics = font.v_metrics(scale);
    for c in chars.chars() {
        let glyph = font.glyph(c).scaled(scale).positioned(point(0., v_metrics.ascent));
        let mut bytes = vec![vec![0; glyphs_width as usize]; glyphs_height as usize];
        if let Some(bounding_box) = glyph.pixel_bounding_box() {
            let x_pad = (glyphs_width as i32 - bounding_box.width()) >> 1;
            let y_pad = (glyphs_height as i32 - bounding_box.height()) >> 1;
            glyph.draw(|x, y, v| {
                let x_i = x as i32 + x_pad;
                let y_i = y as i32 + y_pad;
                if x > 0 && x < glyphs_width && y > 0 && y < glyphs_height {
                    bytes[y_i as usize][x_i as usize] = (v > value_cutoff) as u8 * 255;
                }
            });
        }
        glyph_bytes.push(bytes);
    }

    (glyphs_width, glyph_bytes)
}

// Input is assumed to have only one video stream
// Font used may by ttf or otf
// Destination format is only known to support .mp4
// Codec is H.264
// Pixel format is YUV420p

fn main() {
    let src = "src.mp4";
    let dst = "dst.mkv";
    let mut dst_h = 1080;
    let render_h = 59;
    let font_path = include_bytes!("../MonospaceTypewriter.ttf");
    let char_set = " .-^:~/*+=?%##&$$@@@@@@@@@@@@";

    let start_t = Instant::now();
    let mut last_t = Instant::now();
    ffmpeg_the_third::init().unwrap();
    let is_mkv = dst.ends_with(".mkv");

    // Input
    let mut in_ctx = format::input(src).unwrap();

    // Finds video stream
    let in_vid_stream = in_ctx.streams().best(media::Type::Video)
        .expect("Could not find a proper video stream");
    let in_vid_stream_idx = in_vid_stream.index();
    let in_vid_tb = in_vid_stream.time_base();

    // Creates decoder
    let decoder_ctx = codec::Context::from_parameters(in_vid_stream.parameters()).unwrap();
    let mut decoder = decoder_ctx.decoder().video().unwrap();
    if decoder.format() != Pixel::YUV420P {
        panic!("Pixel format is not YUV420P");
    };

    // Relevant data
    let src_w = decoder.width();
    let src_h = decoder.height();
    let mut dst_w = dst_h * src_w / src_h;
    dst_w -= dst_w % 2;
    dst_h -= dst_h % 2;
    let font_h = dst_h / render_h;
    let src_fmt = decoder.format();
    
    // Font
    let (font_w, char_set) = construct_char_set(font_path, char_set, font_h);
    let render_w = dst_w / font_w;

    // Output
    let mut out_ctx = format::output(dst).unwrap();
    let global_header = out_ctx.format().flags().contains(format::Flags::GLOBAL_HEADER);

    // Creates output stream
    let codec = encoder::find(codec::Id::H264).expect("Couldn't find encoding codec");
    let mut out_vid_stream = out_ctx.add_stream(codec).expect("Couldn't create output stream");
    out_vid_stream.set_time_base(if is_mkv {Rational(1, 1000)} else {in_vid_tb});
    let out_vid_stream_idx = out_vid_stream.index();
    let out_vid_tb = out_vid_stream.time_base();

    // Creates encoder
    let mut encoder = codec::context::Context::new_with_codec(codec)
        .encoder().video().unwrap();
    encoder.set_width(dst_w);
    encoder.set_height(dst_h);
    encoder.set_aspect_ratio(decoder.aspect_ratio());
    encoder.set_format(Pixel::YUV420P);
    encoder.set_frame_rate(Some(in_vid_stream.avg_frame_rate()));
    encoder.set_time_base(in_vid_tb);
    encoder.set_bit_rate(25000000);
    
    if global_header {
        encoder.set_flags(codec::Flags::GLOBAL_HEADER);
    }

    let mut x264_opts = Dictionary::new();
    // x264_opts.set("profile", "100");
    // x264_opts.set("speed", "0");
    // x264_opts.set("force-cfr", "1");

    x264_opts.set("crf", "0");

    let mut encoder = encoder
        .open_with(x264_opts)
        .expect("error opening x264 with supplied settings");
    out_vid_stream.set_parameters(Parameters::from(&encoder));
    
    let dst_fmt = encoder.format();

    // Adds other non-video streams
    let mut stream_mapping: Vec<isize> = vec![0; in_ctx.nb_streams() as _];
    let mut in_stream_tbs = vec![Rational(0, 0); in_ctx.nb_streams() as _];
    let mut out_stream_tbs = vec![Rational(0, 0); in_ctx.nb_streams() as _];
    let mut out_stream_idx = 0;
    for (stream_idx, in_stream) in in_ctx.streams().enumerate() {
        let media = in_stream.parameters().medium();
        if stream_idx == in_vid_stream_idx {
            // Only for video stream
            stream_mapping[stream_idx] = out_stream_idx;
        } else if media == media::Type::Audio || media == media::Type::Subtitle {
            // Creates copy streams for audio and subtitle streams
            let mut out_stream = out_ctx.add_stream(encoder::find(codec::Id::None)).unwrap();
            out_stream.set_parameters(in_stream.parameters());
            unsafe {
                (*out_stream.parameters_mut().as_mut_ptr()).codec_tag = 0;
            }
            out_stream_tbs[out_stream_idx as usize] = if is_mkv {Rational(1, 1000)} else {in_stream.time_base()};
            in_stream_tbs[stream_idx] = in_stream.time_base();
            stream_mapping[stream_idx] = out_stream_idx;
        } else {
            // Ignores other streams
            stream_mapping[stream_idx] = -1;
            continue;
        }
        out_stream_idx += 1;
    }

    // Parses video
    let mut scaler = Context::get(
        src_fmt,
        src_w, src_h,
        Pixel::GRAY8,
        render_w, render_h,
        Flags::FAST_BILINEAR,
    ).unwrap();

    out_ctx.set_metadata(in_ctx.metadata().to_owned());
    out_ctx.write_header().unwrap();
    
    let render_data = RenderData::new(render_w, render_h, dst_w, dst_h, font_w, font_h);
    let mut template = frame::Video::new(dst_fmt, dst_w, dst_h);
    template.data_mut(1).fill(127);
    template.data_mut(2).fill(127);
    let mut frame_ct = 0;
    let total_frames = in_vid_stream.frames();
    for (stream, mut packet) in in_ctx.packets().filter_map(Result::ok) {
        // Parses packets that don't have an out stream
        let in_stream_idx = stream.index();
        let out_stream_idx = stream_mapping[in_stream_idx];
        if out_stream_idx < 0 {
            continue;
        }

        if in_stream_idx == in_vid_stream_idx {
            decoder.send_packet(&packet).unwrap();
            decode_frames(&mut decoder, &mut encoder, &mut scaler, &char_set, &render_data, &template);
            encode_frames(&mut encoder, out_vid_stream_idx, in_vid_tb, out_vid_tb, &mut out_ctx, &mut frame_ct);
        } else {
            packet.rescale_ts(in_stream_tbs[in_stream_idx], out_stream_tbs[out_stream_idx as usize]);
            packet.set_position(-1);
            packet.set_duration(1); // not really sure if this is right
            packet.set_stream(out_stream_idx as usize);
            packet.write_interleaved(&mut out_ctx).unwrap();
        }

        // Logging
        if Instant::now().duration_since(last_t).as_secs_f32() > 5. {
            println!("{}/{} frames processed", frame_ct, total_frames);
            last_t = Instant::now();
        }
    }

    // Close file
    decoder.send_eof().unwrap();
    decode_frames(&mut decoder, &mut encoder, &mut scaler, &char_set, &render_data, &template);
    encoder.send_eof().unwrap();
    encode_frames(&mut encoder, out_vid_stream_idx, in_vid_tb, out_vid_tb, &mut out_ctx, &mut frame_ct);
    out_ctx.write_trailer().unwrap();

    let elapsed_time = start_t.elapsed();
    println!("Took {} seconds.", elapsed_time.as_secs_f32());
}

// fix compression issues