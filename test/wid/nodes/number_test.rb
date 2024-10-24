# frozen_string_literal: true

module Wid
  module Nodes
    class NumberTest < Test
      def test_integer
        assert_equal(1, Wid::Nodes::NumericLiteral.new("1").value)
      end

      def test_float
        assert_equal(1.0, Wid::Nodes::NumericLiteral.new("1.0").value)
      end
    end
  end
end
