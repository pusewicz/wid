module Wid
  module Nodes
    class IfStatement < Node
      attr_reader :test, :consequent, :alternate

      def initialize(test, consequent, alternate)
        @test = test
        @consequent = consequent
        @alternate = alternate
      end

      def children
        [@test, @consequent, @alternate]
      end

      def to_hash
        {class: self.class, test: @test.to_hash, consequent: @consequent.to_hash, alternate: @alternate&.to_hash}
      end
    end
  end
end
