use std::time::Instant;

use ffmpeg_the_third::{codec::{self, Parameters}, decoder, encoder, ffi::AV_TIME_BASE, format::{self, Pixel}, frame, media, packet, software::scaling::{Context, Flags}, Dictionary, Packet, Rational};
use rusttype::{point, Font, Scale};

struct RenderData {
    f_w: usize,
    _f_h: usize,
    r_w: usize,
    r_h: usize,
    dst_w: u32,
    dst_h: u32,
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
            dst_w,
            dst_h,
            x: (0..r_w).map(|x| (x as f32 * dst_w as f32 / r_w as f32) as usize).collect(),
            y: (0..r_h).map(|x| (x as f32 * dst_h as f32 / r_h as f32) as usize).collect(),
        }
    }
}

struct Decoder<'a> {
    decoder: decoder::Video,
    scaler: Context,
    char_set: &'a [Vec<Vec<u8>>],
    new_frame: frame::Video,
    render_data: RenderData,
}
impl<'a> Decoder<'a> {
    fn new(decoder: decoder::Video, render_data: RenderData, scaler: Context, char_set: &'a [Vec<Vec<u8>>], dst_fmt: Pixel) -> Self {
        let mut new_frame = frame::Video::new(dst_fmt, render_data.dst_w, render_data.dst_h);
        new_frame.data_mut(1).fill(127);
        new_frame.data_mut(2).fill(127);

        Self {
            decoder,
            scaler,
            char_set,
            new_frame,
            render_data,
        }
    }

    fn decode_frames(&mut self, encoder: &mut encoder::Video)  {
        let mut frame = frame::Video::empty();
        let lum_to_char = self.char_set.len() as f32 / 256.;
        while self.decoder.receive_frame(&mut frame).is_ok() {
            // Scale frame to render resolution
            let mut scaled_frame = frame::Video::new(Pixel::GRAY8, self.render_data.r_w as u32, self.render_data.r_h as u32);
            self.scaler.run(&frame, &mut scaled_frame).unwrap();
            let padding = scaled_frame.stride(0) - self.render_data.r_w;
            let luminosity = scaled_frame.data_mut(0);

            // Render characters on to output frame
            let stride = self.new_frame.stride(0);
            let bytes = self.new_frame.data_mut(0);
            let mut i = 0;
            for y in self.render_data.y.iter() {
                for x in self.render_data.x.iter() {
                    let char_idx = (luminosity[i] as f32 * lum_to_char) as usize;
                    let stamp = &self.char_set[char_idx];

                    let mut start = x + y*stride;
                    for line in stamp {
                        bytes[start..(start + self.render_data.f_w)].copy_from_slice(line);
                        start += stride;
                    }
                    i += 1;
                }
                i+= padding;
            }

            self.new_frame.set_pts(frame.timestamp());
            encoder.send_frame(&self.new_frame).unwrap();
        }
    }

    fn send_packet<T: packet::Ref>(&mut self, packet: T) {
        self.decoder.send_packet(&packet).unwrap()
    }
    
    fn end(&mut self, encoder: &mut encoder::Video) {
        self.decoder.send_eof().unwrap();
        self.decode_frames(encoder);
    }
}

fn encode_frames(encoder: &mut encoder::Video, out_vid_stream_idx: usize, in_vid_tb: Rational, out_vid_tb: Rational, out_ctx: &mut format::context::Output, frame_ct: &mut u32) {
    let mut encoded = Packet::empty();
    while encoder.receive_packet(&mut encoded).is_ok() {
        encoded.set_stream(out_vid_stream_idx);
        encoded.rescale_ts(in_vid_tb, out_vid_tb);
        encoded.set_flags(packet::Flags::KEY);
        encoded.write_interleaved(out_ctx).unwrap();
        *frame_ct += 1;
    }
}

fn construct_char_set(font_path: &[u8], chars: &str, font_h: u32) -> (u32, Vec<Vec<Vec<u8>>>) {
    // Loads font
    let font = Font::try_from_bytes(font_path).expect("Error constructing Font");

    // Determines proper font scaling
    let value_cutoff = 0.25;
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
// Destination format is only known to support .mp4 and .mkv
// Codec is H.264
// Pixel format is YUV420p
// Requires FFMPEG 5.x.x
fn main() {
    let src = "huh.mkv";
    let dst = "dst.mkv";
    let mut dst_h = 1080;
    let render_h = 60;
    let font_path = include_bytes!("../MonospaceTypewriter.ttf");
    let char_set = " .-:^~=/*+?%##&$$@@@@@@@@@@@@";

    let start_t = Instant::now();
    let mut last_t = Instant::now();
    ffmpeg_the_third::init().unwrap();
    let src_mkv = src.ends_with(".mkv");
    let dst_mkv = dst.ends_with(".mkv");

    // Input
    let mut in_ctx = format::input(format!("./input/{src}")).unwrap();

    // Finds video stream
    let in_vid_stream = in_ctx.streams().best(media::Type::Video)
        .expect("Could not find a proper video stream");
    let in_vid_stream_idx = in_vid_stream.index();
    let in_vid_tb = in_vid_stream.time_base();

    // Creates decoder
    let decoder_ctx = codec::Context::from_parameters(in_vid_stream.parameters()).unwrap();
    let decoder = decoder_ctx.decoder().video().unwrap();

    // Check inputs
    assert!(render_h != 0, "render_h must be greater than 0");
    if src_mkv && !dst_mkv {println!("Transcoding .mkv with subs to mp4 may have undefined behavior")};

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
    let mut out_ctx = format::output(format!("./output/{dst}")).unwrap();
    let global_header = out_ctx.format().flags().contains(format::Flags::GLOBAL_HEADER);

    // Creates output stream
    let codec = encoder::find(codec::Id::H264).expect("Couldn't find encoding codec");
    let mut out_vid_stream = out_ctx.add_stream(codec).expect("Couldn't create output stream");
    out_vid_stream.set_time_base(if dst_mkv {Rational(1, 1000)} else {in_vid_tb});
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
    
    if global_header {
        encoder.set_flags(codec::Flags::GLOBAL_HEADER);
    }

    let mut x264_opts = Dictionary::new();
    x264_opts.set("crf", "24");
    x264_opts.set("g", "60");

    let mut encoder = encoder
        .open_with(x264_opts)
        .expect("error opening x264 with supplied settings");
    out_vid_stream.set_parameters(Parameters::from(&encoder));
    out_vid_stream.set_metadata(in_vid_stream.metadata().to_owned());

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
        } else if media != media::Type::Video && media != media::Type::Unknown {
            // Creates copy streams for audio and subtitle streams
            let mut out_stream = out_ctx.add_stream(encoder::find(codec::Id::None)).unwrap();
            out_stream.set_parameters(in_stream.parameters());
            out_stream.set_metadata(in_stream.metadata().to_owned());
            unsafe {
                (*out_stream.parameters_mut().as_mut_ptr()).codec_tag = 0;
            }
            out_stream_tbs[out_stream_idx as usize] = if dst_mkv {Rational(1, 1000)} else {in_stream.time_base()};
            in_stream_tbs[stream_idx] = in_stream.time_base();
            stream_mapping[stream_idx] = out_stream_idx;
        } else {
            // Ignores other streams
            stream_mapping[stream_idx] = -1;
            continue;
        }
        out_stream_idx += 1;
    }

    // Write header
    for chapter in in_ctx.chapters() {
        if let Some(title) = chapter.metadata().get("title") {
            out_ctx.add_chapter(chapter.id(), chapter.time_base(), chapter.start(), chapter.end(), title).expect("Could not add chapter");
        }
    }
    out_ctx.set_metadata(in_ctx.metadata().to_owned());
    out_ctx.write_header().expect("Could not write header");
    
    // Create transcoding data structures
    let scaler = Context::get(
        src_fmt,
        src_w, src_h,
        Pixel::GRAY8,
        render_w, render_h,
        Flags::FAST_BILINEAR,
    ).unwrap();
    let render_data = RenderData::new(render_w, render_h, dst_w, dst_h, font_w, font_h);
    let mut decoder = Decoder::new(decoder, render_data, scaler, &char_set, dst_fmt);

    // Get total frames
    let mut frame_ct = 0;
    let mut total_frames = in_vid_stream.frames();
    if total_frames == 0 {
        total_frames = in_ctx.duration()
        / AV_TIME_BASE as i64
        * in_vid_stream.avg_frame_rate().numerator() as i64
        / in_vid_stream.avg_frame_rate().denominator() as i64;
    }

    // Parses video
    for (stream, mut packet) in in_ctx.packets().filter_map(Result::ok) {
        // Parses packets that don't have an out stream
        let in_stream_idx = stream.index();
        let out_stream_idx = stream_mapping[in_stream_idx];
        if out_stream_idx < 0 {
            continue;
        }

        if in_stream_idx == in_vid_stream_idx {
            decoder.send_packet(packet);
            decoder.decode_frames(&mut encoder);
            encode_frames(&mut encoder, out_vid_stream_idx, in_vid_tb, out_vid_tb, &mut out_ctx, &mut frame_ct);
        } else {
            packet.rescale_ts(in_stream_tbs[in_stream_idx], out_stream_tbs[out_stream_idx as usize]);
            packet.set_position(-1);
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
    decoder.end(&mut encoder);
    encoder.send_eof().unwrap();
    encode_frames(&mut encoder, out_vid_stream_idx, in_vid_tb, out_vid_tb, &mut out_ctx, &mut frame_ct);
    out_ctx.write_trailer().unwrap();

    let elapsed_time = start_t.elapsed();
    println!("Took {} seconds for {} frames.", elapsed_time.as_secs_f32(), total_frames);
}