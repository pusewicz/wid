# frozen_string_literal: true

require "strscan"

module Wid
  class Tokenizer
    Token = Data.define(:type, :value) do
      def to_a
        [type, value]
      end
    end

    IGNORE = /[ \c\r\t]+/
    INT = /[-]?(?:[0]|[1-9][0-9]*)/
    FLOAT_DECIMAL = /[.][0-9]+/
    FLOAT_EXP = /[eE][+-]?[0-9]+/
    OPERATORS = %w[+ -]
    KEYWORDS = %w[].freeze
    SINGLE_QUOTED_STRING = /'[^']*'/
    DOUBLE_QUOTED_STRING = /"[^"]*"/

    SPEC = {
      NUMBER: /#{INT}(#{FLOAT_DECIMAL}#{FLOAT_EXP}|#{FLOAT_DECIMAL}|#{FLOAT_EXP})?/,
      OPERATOR: /[#{OPERATORS.sort.join}]/,
      STRING: /#{SINGLE_QUOTED_STRING}|#{DOUBLE_QUOTED_STRING}/,
      BOOL: /true|false/,
      NIL: /nil/
    }.freeze

    def initialize(string)
      @string = string
      @scan = StringScanner.new string
      @start = @scan.pos
    end

    def next_token
      @scan.skip(IGNORE)

      return if @scan.eos?

      @start = @scan.pos

      SPEC.each do |type, regexp|
        if (value = @scan.scan(regexp))
          return Token.new(type:, value:)
        end
      end

      raise "Unrecognized token #{@scan.getch.inspect}"
    end

    def line
      @scan.string[0, @scan.pos].count("\n") + 1
    end
  end
end
